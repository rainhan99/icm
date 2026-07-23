# F-003b — Postgres row-level tenant isolation (RLS)

Turn the identity scaffold (F-003: token→tenant) and the pre-seeded tenant
columns (F-003a) into a real **cross-tenant data isolation** boundary on the
central Postgres node: each tenant reads and writes only its own rows,
enforced by PostgreSQL Row-Level Security.

Predecessors (all done): F-001 (three-tier shared memory), F-002 (code
graph), F-003 (supersession + token→tenant auth scaffold), F-003a (PG full
subsystem parity + nullable `tenant` columns on the 4 new tables).

## Goal / Scope

Today a shared Postgres node serves **one dataset** — the `tenant` columns
exist (F-003a) but are unused, and F-003's token→tenant map resolves
*identity* only. F-003b closes the loop: the HTTP layer sets the resolved
tenant on the connection, and **PostgreSQL RLS** filters every read/write to
that tenant. Enforcement lives in the database, so an app-code mistake
cannot leak across tenants.

Design decisions (user-approved in brainstorming):
- **Enforcement:** PostgreSQL RLS (`ENABLE` + `FORCE ROW LEVEL SECURITY` +
  policies keyed on `current_setting('app.tenant', true)`).
- **Tenant injection:** session-level `SET` on the per-request-locked
  connection (safe because the `Mutex<Client>` serializes access; a future
  connection pool must switch to `SET LOCAL` — documented).
- **Unset tenant → unrestricted** (today's behavior): default/SQLite/single-
  token/no-tokens-map deployments are byte-for-byte unchanged.
- **Migration:** backfill existing `NULL`-tenant rows to the `'default'`
  tenant.

### Scope Checklist

- **SC-1** — `memories` gains a nullable `tenant` column on the **PG** backend
  (F-003a deferred it), `DEFAULT current_setting('app.tenant', true)` so
  writes auto-tag; idempotent `ADD COLUMN IF NOT EXISTS`. SQLite `memories`
  is unchanged.
- **SC-2** — The seven F-003a PG tables (facts, memoirs, concepts,
  concept_links, feedback, sessions, messages) get the same
  `DEFAULT current_setting('app.tenant', true)` on their existing `tenant`
  column so inserts auto-tag without changing INSERT statements.
- **SC-3** — `ENABLE ROW LEVEL SECURITY` + `FORCE ROW LEVEL SECURITY` on all
  eight tenant-scoped PG tables (idempotent).
- **SC-4** — RLS policies (`USING` + `WITH CHECK`) on all eight tables:
  a row is visible/writable iff `tenant = current_setting('app.tenant', true)`
  **OR** the setting is unset/empty (`NULL`/`''` → unrestricted). Unset =
  today's single-dataset behavior.
- **SC-5** — `Store::set_tenant(&self, Option<&str>)` (inherent enum method):
  the PG backend runs an **injection-safe** `SELECT set_config('app.tenant',
  $1, false)` (and clears to `''` for `None`); SQLite and other backends are
  a no-op. The GUC is NEVER built by string interpolation.
- **SC-6** — HTTP wiring: every store-serving handler (`/rpc` and the REST
  `/recall`, `/store`, `/consolidate`, `/stats`, `/topics`) reads the
  resolved tenant from the request extensions (F-003 `Tenant`) and calls
  `store.set_tenant(...)` on the locked store **before** dispatching.
- **SC-7** — Migration: backfill existing `NULL`-tenant rows to `'default'`
  on every tenant table (idempotent `UPDATE … WHERE tenant IS NULL`), run
  **before** `FORCE` so it is not blocked by the policy.
- **SC-8** — Isolation tests (gated on `ICM_POSTGRES_URL`): two tenants store
  data; tenant A's recall/list/get sees only A's rows, B only B's;
  cross-tenant `get(id)` of B's row under A returns `None`; writes auto-tag;
  unset tenant sees both (unrestricted).
- **SC-9** — Backward-compat / zero-regression: unset tenant = today's
  behavior; SQLite backend unchanged (no RLS, single dataset, no `tenant`
  column added); default `cargo test --workspace` green; the GUC set is
  parameterized (grep proves no interpolated `SET app.tenant`).
- **SC-10** — Docs: `docs/postgres-backend.md` flips "Multi-tenancy: not yet"
  to an RLS setup section (session-`SET`/pooling caveat + "SQLite is not
  isolated" + "direct CLI/admin access is unrestricted"); `docs/remote-
  backend.md` §6 updates the F-003 non-isolation warning to point at F-003b
  as delivered (PG only).

### In scope

- SC-1..SC-10: DB-enforced per-tenant isolation on the PG backend, wired from
  the HTTP tenant identity, backward-compatible and injection-safe.

### Out of scope (deferred / limitations)

- **SQLite isolation** — SQLite has no RLS; it stays single-dataset. Isolation
  is a PG-only guarantee (documented).
- **Direct CLI/admin path** — `icm` run directly against PG (not via the HTTP
  node) runs with tenant unset = unrestricted (trusted-admin view). The
  multi-tenant boundary is the HTTP-served path (thin clients).
- **Connection pooling** — the session-`SET` approach assumes the current
  single guarded connection; a pool would require `SET LOCAL` (documented,
  not built).
- Per-tenant quotas / RBAC / cross-tenant admin dashboards; OpenSearch
  tenant isolation.

### Corner cases

- Injection: tenant name with quotes/semicolons must be inert — enforced by
  parameterized `set_config`, never interpolation.
- Unset vs empty GUC: `current_setting('app.tenant', true)` returns `NULL`
  when never set and `''` when reset; policies treat both as unrestricted.
- `FORCE ROW LEVEL SECURITY` so the table-owner connection is also filtered
  (RLS is otherwise bypassed for the owner).
- Migration idempotency: re-running backfill + `ENABLE`/`FORCE`/policy
  creation is a no-op.
- Writes auto-tag via column DEFAULT; an explicit tenant in a future INSERT
  must still pass `WITH CHECK`.
- Concurrent requests: the `Mutex<Client>` serializes, so a session `SET`
  cannot bleed across interleaved requests.
- Session `SET` persists across a transaction — fine, all queries in one
  handler share the tenant.

## Architecture / Context dependencies

- **Auth → store handoff:** F-003's `auth_middleware` already injects the
  resolved `Tenant` into request extensions. F-003b extracts it in the
  handlers and calls `store.set_tenant(...)` on the store locked for that
  request (`state.store.lock()` in `http_api.rs`), before
  `rpc_dispatch::dispatch` / the REST handler body runs. `dispatch` stays
  tenant-agnostic.
- **`Store::set_tenant`** is an inherent method on the `Store` enum
  (`backend.rs`) that matches the backend: PG issues `set_config`; SQLite /
  OpenSearch are no-ops. Not part of the `MemoryStore` trait (it is a
  connection concern, not a memory operation).
- **Schema/RLS** live in `PostgresStore::init_schema` (extend with the
  memories `tenant` column, the auto-tag DEFAULTs, the NULL→'default'
  backfill, and the `ENABLE`/`FORCE`/`CREATE POLICY` statements — all
  idempotent). Ordering: columns → backfill → enable+force+policies.
- **Sync store invariant** holds — `set_config` is one blocking query; no
  async introduced. SQLite path untouched.

## Environment requirements

- **No new dependencies** — RLS and `set_config` are PostgreSQL built-ins;
  `postgres`/`pgvector` already present.
- **Opt-in, non-default** — all changes are under the `postgres` feature; the
  default SQLite build and its behavior are unchanged.
- **Test PG** — the F-003a harness (`ICM_POSTGRES_URL`, docker
  `pgvector/pgvector:pg16` on host port 5433) is reused; new isolation tests
  are gated the same way. The optional CI PG job runs them.
- **Version pins** — none changed; RLS works on all supported PG versions.

## Definition of Done delta

Appended to the project contract's Definition of Done for F-003b:

1. `ICM_POSTGRES_URL=… cargo test -p icm-store --features postgres` includes
   RLS isolation tests: tenant A cannot read/get/list tenant B's rows; writes
   auto-tag; unset tenant sees all.
2. Default `cargo test --workspace` (no `ICM_POSTGRES_URL`) green; SQLite
   behavior byte-identical (no `tenant` column, no RLS).
3. `clippy -D warnings` on default and `--features postgres`; `fmt` clean.
4. GUC is set via parameterized `set_config` — no interpolated
   `SET app.tenant` in the source (grep-verified).
5. `init_schema` idempotent: re-running (columns + backfill + RLS + policies)
   is a no-op.
6. Existing NULL-tenant rows migrate to `'default'`.
7. `docs/postgres-backend.md` documents RLS multi-tenancy (+ pooling caveat,
   SQLite/CLI non-isolation); `docs/remote-backend.md` §6 updated.

## Handoff state requirements

- **Multi-tenant story complete:** F-003 (identity) + F-003a (parity) +
  F-003b (isolation) → production-ready multi-tenant PG. `.zeus/features.md`
  F-003b → done.
- Docs reflect the delivered boundary and its limits (PG-only, HTTP-path,
  pooling caveat).
- Observability: tenant already logged at auth; store ops run under the set
  tenant. No token ever logged.
- Delivery via B-workflow (local merge to `main`, push fork `origin`, no
  upstream PR).

## 7-gate impact map

- **G1 (code)** — `init_schema` RLS/columns/backfill; `Store::set_tenant`
  (enum + PG); HTTP handler wiring.
- **G2 (TDD red-green)** — isolation tests written failing first against a
  live PG (two tenants), then green.
- **G3 (verify)** — `ICM_POSTGRES_URL=… cargo test -p icm-store --features
  postgres rls_…`; grep for injection-safety.
- **G4 (DoD)** — the 7 items above.
- **G5 (E2E)** — central PG node + two F-003 tokens → two tenants → HTTP
  store/recall proven isolated end-to-end.
- **G6 (review)** — RLS policy correctness (USING + WITH CHECK, unset
  semantics), FORCE, injection-safety, zero-regression, no-panic.
- **G7 (handoff)** — features.md + docs updated; multi-tenant story closed.
