# F-003b Implementation Plan — Postgres row-level tenant isolation (RLS)

## Header

- **Goal:** Enforce per-tenant data isolation on the PG backend via
  PostgreSQL Row-Level Security, driven by the tenant the HTTP layer already
  resolves (F-003). Add a `tenant` column to `memories`, auto-tag writes,
  `ENABLE`+`FORCE` RLS with `current_setting('app.tenant')` policies on all
  eight tenant-scoped tables, set the tenant on the per-request-locked
  connection, and backfill legacy rows to `'default'`.
- **Architecture:** `Store::set_tenant` (enum, PG-only) runs an
  injection-safe `set_config('app.tenant', $1, false)`; HTTP handlers call it
  on the locked store before querying; RLS filters at the DB. Unset tenant =
  unrestricted (today's behavior); SQLite unchanged.
- **Tech Stack:** Rust (sync store), PostgreSQL/SQL (RLS, GUC), HTTP API (axum).
- **Feature tag:** F-003b
- **Spec:** `.zeus/specs/2026-07-23-postgres-tenant-rls-design.md`

## File Map

| File | Created/Modified | Responsibility |
|---|---|---|
| `crates/icm-store/src/postgres.rs` | Modified | `init_schema`: add `memories.tenant` (PG) + `DEFAULT current_setting('app.tenant', true)` on all 8 tables' tenant columns + backfill `NULL`→`'default'` + `ENABLE`/`FORCE ROW LEVEL SECURITY` + idempotent policies. `PostgresStore::set_tenant` (parameterized `set_config`). Gated `rls_*` tests. |
| `crates/icm-store/src/backend.rs` | Modified | `Store::set_tenant(&self, Option<&str>)` inherent method — matches `Postgres` (real), all other variants (incl. `Remote`) no-op. |
| `crates/icm-cli/src/http_api.rs` | Modified | Wire the resolved `Tenant` (request extensions) → `store.set_tenant(...)` on the locked store, in the 6 store-serving handlers (`/rpc`, `/recall`, `/store`, `/consolidate`, `/stats`, `/topics`); `run_recall` gains a tenant param so its internal lock sets the tenant. Gated PG-backed two-tenant E2E test. |
| `docs/postgres-backend.md` | Modified | Replace "Multi-tenancy: not yet" with an RLS setup section (session-`SET`/pooling caveat, SQLite-not-isolated, direct-CLI-unrestricted). |
| `docs/remote-backend.md` | Modified | §6: update the F-003 non-isolation warning — isolation now delivered in F-003b (PG only). |
| `.zeus/features.md` | Modified | F-003b → `done` (G7 handoff). |

No new source files. `postgres.rs` grows ~120 lines (schema RLS + set_tenant +
tests) → still one persistence file, consistent with the repo convention (§10).

## Architect Risk Analysis

### Rust (sync store + axum) lens

- **Restate:** a `set_tenant(&self, Option<&str>)` sets the connection's
  tenant; HTTP handlers call it on the store they've locked for the request,
  before querying, so RLS sees the right tenant.
- **Risk — lock split (the critical one):** the set-tenant and the query MUST
  share ONE `state.store.lock()` guard. `handle_recall` delegates to
  `run_recall`, which locks *internally* — if the handler set the tenant
  under its own lock and `run_recall` then re-locked, another request could
  reset the tenant in between → **cross-tenant leak**. Mitigation: thread the
  tenant INTO `run_recall` (and any inner-locking helper) so the tenant is
  set inside the same guard as the query. No handler may set the tenant under
  a lock it then releases before querying.
- **Risk — missing tenant extractor:** store handlers use a required
  `Extension<Tenant>`. The middleware always inserts `Tenant` for non-`/health`
  paths, so it is present; if the invariant ever broke, axum returns 500
  (fail-closed — no data served) rather than serving unrestricted.
- **Risk — Remote backend:** the thin client (`Store::Remote`) must NOT try to
  set RLS; it forwards its token and the central node maps token→tenant.
  `set_tenant` is a no-op on `Remote`/`Sqlite`/`OpenSearch`.
- **Panic discipline:** `set_config` via `try_get`/`execute` + `pg_err`; no
  `unwrap`/`Row::get`.

### PostgreSQL / SQL lens

- **Restate:** RLS policies on 8 tables key on `current_setting('app.tenant',
  true)`; writes auto-tag via a column DEFAULT; unset GUC → unrestricted.
- **Risk — GUC injection:** `SET app.tenant = '<interpolated>'` is injectable.
  MUST use `SELECT set_config('app.tenant', $1, false)` with `$1` bound. The
  DoD greps to prove no interpolated `SET app.tenant` exists.
- **Risk — `current_setting` on unset GUC raises:** must use the **2-arg**
  `current_setting('app.tenant', true)` (missing_ok) which returns `NULL`
  when unset; the 1-arg form errors. Policies treat `NULL`/`''` as
  unrestricted.
- **Risk — FORCE + owner:** RLS is bypassed for the table owner unless
  `FORCE ROW LEVEL SECURITY`. Use FORCE so the boundary holds regardless of
  the connecting role; unset tenant still yields the unrestricted (admin)
  view via the policy.
- **Risk — policy idempotency:** `CREATE POLICY` has no `IF NOT EXISTS`
  (even PG16). Use `DROP POLICY IF EXISTS <name> ON <table>; CREATE POLICY …`
  per table so re-connect is a no-op. `ENABLE`/`FORCE` are already idempotent.
- **Risk — migration ordering:** run the `NULL`→`'default'` backfill BEFORE
  `FORCE` (and it also stays correct after, since unset→unrestricted at
  connect time). Column adds/defaults are `IF NOT EXISTS`.
- **Risk — FK under RLS:** an INSERT into `concepts` FK-references `memoirs`;
  the parent must be visible under the current tenant. Cross-tenant FK
  references would fail — acceptable (a concept and its memoir share a
  tenant). Same-tenant and unset paths are unaffected; noted for reviewers.

*User: confirm this analysis (esp. the lock-split mitigation and
injection-safe `set_config`) — embedded here, covered by the approval gate.*

## Tasks

Provision the ephemeral PG once before T1 (same as F-003a):

```bash
docker run -d --name icm-pg -e POSTGRES_USER=icm -e POSTGRES_PASSWORD=icm \
  -e POSTGRES_DB=icm -p 5433:5432 pgvector/pgvector:pg16
export ICM_POSTGRES_URL="postgres://icm:icm@localhost:5433/icm"
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
```

### T0 — Reconnaissance (no code)

Re-read: `backend.rs` inherent-method pattern + variants; `PostgresStore`
`conn()`/`pg_err`; `http_api.rs` handlers `handle_rpc`/`handle_recall`/
`run_recall`/`handle_store`/`handle_consolidate`/`handle_stats`/`handle_topics`
lock sites + the F-003 `Tenant` extension. No commit.

### T1 — `Store::set_tenant` + `PostgresStore::set_tenant`

1. **Failing test** (`pg_tests`): set/read-back the GUC.
   ```rust
   #[test]
   fn rls_set_tenant_roundtrip() {
       let Some(s) = pg() else { return };
       s.set_tenant(Some("tenant-x")).unwrap();
       assert_eq!(current_setting(&s), "tenant-x");
       s.set_tenant(None).unwrap();          // clears
       assert_eq!(current_setting(&s), "");
       // injection attempt is inert (treated as a literal tenant name).
       s.set_tenant(Some("x'; DROP TABLE memories;--")).unwrap();
       assert!(s.count().is_ok());            // table still there
   }
   // helper: SELECT current_setting('app.tenant', true) → String ("" if unset)
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres rls_set_tenant` → `set_tenant` absent (compile error).
3. **Minimal impl:** `Store::set_tenant(&self, Option<&str>) -> IcmResult<()>`
   in `backend.rs` (match: `Postgres(s) => s.set_tenant(t)`, `_ => Ok(())`).
   `PostgresStore::set_tenant`: `self.conn()?.execute("SELECT set_config('app.tenant', $1, false)", &[&tenant.unwrap_or("")])` mapped via `pg_err`.
4. **Confirm pass / commit:** `feat(store): Store::set_tenant — injection-safe PG session tenant (no-op elsewhere)`.

### T2 — Schema: `memories.tenant` + auto-tag defaults + backfill

1. **Failing test** (`pg_tests`): memories has a tenant column; writes auto-tag.
   ```rust
   #[test]
   fn rls_schema_and_autotag() {
       let Some(s) = pg() else { return };
       reset(&s);
       assert!(table_columns(&s, "memories").iter().any(|c| c == "tenant"));
       s.set_tenant(Some("acme")).unwrap();
       let id = s.store(Memory::new("t".into(), "hi".into(), Importance::High)).unwrap();
       // raw read (bypass filtering by reading the column under the same tenant)
       assert_eq!(tenant_of_row(&s, "memories", &id), "acme");
       s.set_tenant(None).unwrap();
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres rls_schema_and_autotag` → no `memories.tenant`.
3. **Minimal impl:** in `init_schema`, after the tables exist:
   `ALTER TABLE memories ADD COLUMN IF NOT EXISTS tenant TEXT;` and, for all
   eight tables, `ALTER TABLE <t> ALTER COLUMN tenant SET DEFAULT current_setting('app.tenant', true);`
   then backfill `UPDATE <t> SET tenant = 'default' WHERE tenant IS NULL;`
   (before RLS FORCE in T3). Idempotent.
4. **Confirm pass / commit:** `feat(store): memories tenant column + auto-tag defaults + NULL→default backfill (PG)`.

### T3 — RLS enable + force + policies (the isolation boundary)

1. **Failing test** (`pg_tests`): two-tenant isolation (SC-8 core).
   ```rust
   #[test]
   fn rls_isolation_across_tenants() {
       let Some(s) = pg() else { return };
       reset(&s);
       s.set_tenant(Some("A")).unwrap();
       let a = s.store(Memory::new("t".into(), "A secret".into(), Importance::High)).unwrap();
       s.set_tenant(Some("B")).unwrap();
       let b = s.store(Memory::new("t".into(), "B secret".into(), Importance::High)).unwrap();
       // B sees only B.
       assert!(s.get(&a).unwrap().is_none(), "B must not read A's row");
       assert!(s.get(&b).unwrap().is_some());
       let listed = s.list_all().unwrap();
       assert_eq!(listed.len(), 1);
       assert!(listed[0].summary.contains("B secret"));
       // A sees only A.
       s.set_tenant(Some("A")).unwrap();
       assert!(s.get(&b).unwrap().is_none());
       assert_eq!(s.list_all().unwrap().len(), 1);
       // unset → unrestricted (both).
       s.set_tenant(None).unwrap();
       assert_eq!(s.list_all().unwrap().len(), 2);
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres rls_isolation` → without RLS, B sees A's row (assert fails).
3. **Minimal impl:** in `init_schema` (after backfill), for each of the eight
   tables: `ALTER TABLE <t> ENABLE ROW LEVEL SECURITY; ALTER TABLE <t> FORCE ROW LEVEL SECURITY;
   DROP POLICY IF EXISTS icm_tenant_isolation ON <t>;
   CREATE POLICY icm_tenant_isolation ON <t> USING (
       current_setting('app.tenant', true) IS NULL
       OR current_setting('app.tenant', true) = ''
       OR tenant = current_setting('app.tenant', true))
   WITH CHECK (
       current_setting('app.tenant', true) IS NULL
       OR current_setting('app.tenant', true) = ''
       OR tenant = current_setting('app.tenant', true));`
   Update `reset()` to `set_tenant(None)` before truncating (TRUNCATE ignores
   RLS but keep the harness tenant clean between tests).
4. **Confirm pass / commit:** `feat(store): RLS tenant isolation policies on all PG subsystem tables`.

### T4 — HTTP wiring + two-tenant E2E

1. **Failing test** (`http_api` tests, gated + PG-backed): two tokens → two
   tenants → isolated over HTTP.
   ```rust
   #[tokio::test]
   async fn rls_http_two_tenants() {
       let Some(state) = pg_app_state_multi(&[("tokA","tenantA"),("tokB","tenantB")]) else { return };
       let app = build_router(state);
       // tokA stores; tokB stores; each recall sees only its own via /rpc.
       // (store memory.store then memory.list; assert cross-tenant invisibility)
       ...
       assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED); // no token still 401
   }
   ```
2. **Confirm fail:** `cargo test -p icm-cli --features "http-api,remote-store" rls_http_two_tenants` → handlers don't set tenant, so tenantB sees tenantA's rows.
3. **Minimal impl:** add `Extension<Tenant>` to the 6 store handlers; after
   `state.store.lock()`, call `store.set_tenant(Some(&tenant.0))` (map error
   to 500) before dispatch / query. Thread `tenant` into `run_recall` so its
   internal lock sets the tenant in the SAME guard as the search. Add the
   `pg_app_state_multi` gated test helper (PG-backed AppState with a tokens
   map; returns `None` without `ICM_POSTGRES_URL`).
4. **Confirm pass / commit:** `feat(cli): wire resolved tenant into store handlers (RLS end-to-end)`.

### T5 — Docs

1. **Verification:** `grep -qi "row level security\|RLS\|current_setting" docs/postgres-backend.md` && `grep -qi "session.*SET\|pooling\|SET LOCAL" docs/postgres-backend.md` && `grep -qi "F-003b" docs/remote-backend.md`.
2. **Implement:** `docs/postgres-backend.md` — replace the "Multi-tenancy: not
   yet" block with an "RLS multi-tenancy" section: how token→tenant→RLS works,
   the session-`SET`/single-connection + pooling→`SET LOCAL` caveat, "SQLite
   is not isolated", "direct CLI/admin (unset tenant) is unrestricted".
   `docs/remote-backend.md` §6 — update the F-003 non-isolation warning to
   "data isolation delivered in F-003b (PG backend only)".
3. **Commit:** `docs(store): RLS multi-tenancy (postgres) + F-003b isolation note`.

### T6 — Full gate + zero-regression

1. **Verification:**
   - `cargo fmt --all -- --check`
   - `cargo clippy --workspace --all-targets -- -D warnings` (default)
   - `cargo clippy -p icm-store --features postgres --all-targets -- -D warnings`
   - `env -u ICM_POSTGRES_URL cargo test --workspace` → green; PG tests skip; SQLite byte-identical (no tenant column, no RLS).
   - `ICM_POSTGRES_URL=… cargo test -p icm-store --features postgres pg_ -- --test-threads=1` → all F-003a `pg_*` (unset → unrestricted) AND new `rls_*` green.
   - `ICM_POSTGRES_URL=… cargo test -p icm-cli --features "http-api,remote-store" rls_http -- --test-threads=1` → E2E green.
   - injection grep: `! grep -rn "SET app.tenant" crates/` (only `set_config` is used).
   - migration idempotency: connect twice against the same DB (second `pg()` call) → no error (implicit in every gated test).
2. **Implement:** fix any gate issue.
3. **Commit:** `chore(f-003b): verify gates + zero-regression`.

## Test Plan

- **Unit (gated `pg_tests`):** `rls_set_tenant_roundtrip` (T1: set/clear/inject-inert),
  `rls_schema_and_autotag` (T2: column + DEFAULT auto-tag),
  `rls_isolation_across_tenants` (T3: store/get/list isolation + unset=all).
- **Integration:** the F-003a `pg_*` suite re-run under the new RLS schema
  (unset tenant → unrestricted → still green) proves subsystem parity holds
  with RLS enabled.
- **E2E (gated, PG-backed):** `rls_http_two_tenants` — two F-003 tokens →
  two tenants over `/rpc`; each tenant's `memory.list` returns only its own
  rows; missing token still `401`.
- **Regression:** `env -u ICM_POSTGRES_URL cargo test --workspace` — PG tests
  skip, SQLite unchanged (no `tenant` column, no RLS); existing http_api
  SQLite tests still pass (SQLite `set_tenant` is a no-op).

## Security Review

- **Static:** `cargo clippy --features postgres -- -D warnings`; `cargo audit`
  (no new deps).
- **Threat model (this IS the security feature):**
  - **GUC injection:** tenant only ever reaches SQL through
    `set_config('app.tenant', $1, false)` ($1 bound). Named check:
    `! grep -rn "SET app.tenant" crates/` and no `format!`-built tenant SQL.
  - **Cross-tenant read/write:** enforced by `ENABLE`+`FORCE` RLS + USING &
    WITH CHECK on all eight tables — a forgotten app-level filter cannot leak.
    Verified by `rls_isolation_across_tenants` (get + list) and the HTTP E2E.
  - **Lock-split race:** set-tenant and query share one store guard (see
    Architect Risk); `run_recall` takes the tenant so its inner lock sets it.
  - **Fail-closed on missing tenant:** required `Extension<Tenant>` → 500 (no
    data) if absent, never unrestricted, in the HTTP path.
  - **FORCE** ensures the owner role is also constrained.
- **Specific checks:** input validation (bound GUC), secrets (no token in
  logs — unchanged; only tenant), auth (F-003 401 path intact), injection
  (parameterized), no SSRF/path traversal.

## Logic Review Checkpoints

- **CP-A (after T1/T2):** `set_config` parameterized (no interpolation);
  `current_setting(…, true)` 2-arg everywhere; `memories.tenant` added on PG
  only (SQLite untouched); DEFAULT auto-tags; backfill runs before FORCE.
- **CP-B (after T3):** policy USING **and** WITH CHECK present on all 8
  tables; unset (`NULL`/`''`) → unrestricted; `FORCE` set; policy creation
  idempotent (DROP IF EXISTS + CREATE); F-003a `pg_*` suite still green under
  RLS.
- **CP-C (after T4):** every store handler sets the tenant under the SAME lock
  as its query; `run_recall` sets it inside its own lock; `Remote`/`Sqlite`
  `set_tenant` no-op; missing tenant → 500 not unrestricted.
- **CP-D (docs, T5):** RLS scope stated honestly (PG-only, HTTP-path,
  pooling caveat, CLI unrestricted).

## G4 contract delta

Appended to the project contract's Definition of Done after this plan:

1. `ICM_POSTGRES_URL=… cargo test -p icm-store --features postgres` includes
   RLS isolation tests (A cannot read/get/list B's rows; unset sees all).
2. `ICM_POSTGRES_URL=… cargo test -p icm-cli --features "http-api,remote-store" rls_http`
   proves two tokens → two isolated tenants over HTTP.
3. `env -u ICM_POSTGRES_URL cargo test --workspace` green; SQLite byte-identical.
4. No interpolated `SET app.tenant` in the source (only parameterized
   `set_config`); `clippy -D warnings` (default + postgres); `fmt` clean.
5. `init_schema` idempotent (columns + backfill + RLS + policies re-run = no-op).
6. Legacy `NULL`-tenant rows migrated to `'default'`.
7. `docs/postgres-backend.md` RLS section + `docs/remote-backend.md` §6 updated.

## Logic Completeness Manifest

**Every requirement in the linked spec MUST be implemented in full.
Authorized simplifications: (none).**

Scope honesty (NOT a cut — spec-declared): RLS isolation is PG-only (SQLite
has no RLS → single dataset) and enforced on the HTTP-served path (direct
CLI/admin runs unset = unrestricted). These are documented boundaries, not
dropped requirements.

### Spec Coverage Matrix

| SC-ID | Capability | Implementing task(s) | Verification command |
|---|---|---|---|
| SC-1 | `memories.tenant` column + auto-tag DEFAULT (PG) | T2 | `ICM_POSTGRES_URL=… cargo test -p icm-store --features postgres rls_schema_and_autotag` |
| SC-2 | Auto-tag DEFAULT on the 7 F-003a tables | T2 | same as SC-1 (`rls_schema_and_autotag`) |
| SC-3 | `ENABLE`+`FORCE ROW LEVEL SECURITY` on 8 tables | T3 | `… rls_isolation_across_tenants` |
| SC-4 | RLS USING+WITH CHECK policies (unset=unrestricted) | T3 | `… rls_isolation_across_tenants` |
| SC-5 | `Store::set_tenant` (injection-safe PG; no-op else) | T1 | `… rls_set_tenant_roundtrip` |
| SC-6 | HTTP handlers set tenant on the locked store | T4 | `cargo test -p icm-cli --features "http-api,remote-store" rls_http_two_tenants` |
| SC-7 | Backfill `NULL`→`'default'` | T2 | `… rls_schema_and_autotag` (+ migration note) |
| SC-8 | Two-tenant isolation tests | T3, T4 | `… rls_isolation_across_tenants` + `rls_http_two_tenants` |
| SC-9 | Zero-regression + injection-safe | T6 | `env -u ICM_POSTGRES_URL cargo test --workspace` + `! grep -rn "SET app.tenant" crates/` |
| SC-10 | Docs (postgres RLS + remote §6) | T5 | `grep -qi "row level security" docs/postgres-backend.md` + `grep -qi F-003b docs/remote-backend.md` |

All SC-1..SC-10 mapped. Manual coverage check (no
`scripts/check-spec-coverage.sh`) — declared degraded/manual, no orphans.

## File Size Constraints

| File | Projected | Threshold | Flag |
|---|---|---|---|
| `crates/icm-store/src/postgres.rs` | ~2900 lines (from ~2780; +set_tenant, +RLS DDL, +3 tests) | Persistence backend (relaxed) | OK (relaxed) — under the SQLite `store.rs` precedent |
| `crates/icm-store/src/backend.rs` | +~10 lines | Dispatch/routing (relaxed) | OK (relaxed) |
| `crates/icm-cli/src/http_api.rs` | +~40 lines (6 handler tweaks + 1 gated test) | Service/controller | OK |
| `docs/postgres-backend.md` | ~205 lines | Docs (relaxed) | OK (relaxed) |
| `docs/remote-backend.md` | ±5 lines | Docs (relaxed) | OK (relaxed) |
| `.zeus/features.md` | +~8 lines | Docs (relaxed) | OK (relaxed) |

No `OVER` rows.

**User-approved:** 2026-07-23 by rainhan@coupert.com
