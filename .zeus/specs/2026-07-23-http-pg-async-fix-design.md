# F-003c — HTTP/web + Postgres async-blocking fix (store-thread actor)

Make the axum HTTP servers (`icm serve --http` and the `--web` dashboard)
work on the **Postgres** backend. Today the first store operation panics —
`Cannot start a runtime from within a runtime` — because the blocking
`postgres` 0.19 client is driven synchronously from a tokio runtime worker
(its internal `block_on` cannot nest inside tokio). Discovered in the F-003b
end-to-end demo.

> **Revision (execution finding).** The first-drafted approach —
> `tokio::task::spawn_blocking` — was proven insufficient: tokio **enters the
> runtime context on its blocking-pool threads too** (`Handle::current()`
> succeeds there), so the postgres client's `Runtime::block_on` still panics.
> Only a thread tokio does **not** manage escapes it. The design below is
> therefore a **dedicated store-owning `std::thread` (actor)** reached over a
> channel — user-approved pivot from the original spawn_blocking plan.

This is **not** an RLS/F-003b defect — the store logic and RLS are correct
(store-layer tests + a live `psql` demo prove isolation). It is a
pre-existing gap: `http-api`/`web` (tokio) and `postgres` (blocking client)
were never exercised together (F-001's HTTP tests use SQLite).

Predecessors (all done): F-001, F-002, F-003, F-003a, F-003b.

## Goal / Scope

Move every synchronous store operation performed by an axum handler onto a
**dedicated store-owning `std::thread` (actor)** reached over a channel, so
the blocking store client never runs on a tokio-managed thread (async worker
*or* blocking pool — both carry the runtime context that makes its `block_on`
panic). The store trait stays **synchronous** (CLAUDE.md invariant) — only
where it runs changes. This unblocks the production topology: thin clients →
HTTP central node → Postgres (multi-tenant, 200 users).

Design decisions (user-approved; revised after the spawn_blocking finding):
- **Approach:** a **dedicated store-owning `std::thread` (actor)**. One
  long-lived OS thread — outside tokio's control, so it has no runtime
  context and the postgres client's `block_on` works — owns the `Store` (and
  the embedder). A cloneable `StoreHandle` lets async handlers submit a job
  (a `FnOnce(&Store, Option<&dyn Embedder>)`) over a channel and `.await` the
  result via a oneshot. (Option ③ async `tokio-postgres` was rejected — it
  breaks the sync-store invariant; `spawn_blocking` was rejected after being
  proven insufficient.)
- **Serialization:** the single actor thread processes one job at a time —
  it *replaces* the `Arc<Mutex<Store>>` as the serialization point (the store
  is owned by the thread, not shared).
- **Uniform:** all backends go through the actor (not PG-only) — no
  backend-branching in handlers; SQLite behavior is unchanged.
- **Scope:** fix **both** axum servers — `http_api.rs` and `web.rs`.

### Scope Checklist

- **SC-1** — A store-actor: one dedicated `std::thread` owns the `Store` (+
  embedder) and runs submitted jobs; a cloneable `StoreHandle` submits a
  `FnOnce(&Store, Option<&dyn Embedder>)` over a channel and returns the
  result via a oneshot. The thread is not tokio-managed, so the postgres
  client's `block_on` works → no "runtime within a runtime" panic. Every
  store-serving `http_api.rs` handler (`/rpc`, `/recall`, `/store`,
  `/consolidate`, `/stats`, `/topics`) runs its store work through it.
- **SC-2** — F-003b invariant preserved: the tenant `set_tenant` and the
  query it scopes run in the **same** actor job (the thread processes one job
  at a time), so tenant scoping over HTTP+PG stays correct and un-interleaved.
- **SC-3** — Every store-locking `web.rs` dashboard handler runs its store
  work through the actor handle, so `icm serve --web` works over Postgres.
  (The dashboard adds **no** tenant wiring — an admin view runs with tenant
  unset = unrestricted, which is the intended semantics.)
- **SC-4** — Uniform across backends (no PG-only branch); existing SQLite
  `http_api`/`web` tests remain green (the actor is transparent to the sync
  SQLite path).
- **SC-5** — Panic containment: a job that panics does not crash the server —
  the actor stays alive (or the dropped oneshot yields an error) and the
  handler returns HTTP 500.
- **SC-6** — Gated HTTP+PG test: a store operation over a Postgres-backed
  axum app returns success (HTTP 200 with a valid result), **not** a panic —
  gated on `ICM_POSTGRES_URL`, skips cleanly without it.
- **SC-7** — Zero-regression + hygiene: default (SQLite) `cargo test
  --workspace` green; `clippy -D warnings` on default and on
  `http-api,web,postgres,remote-store`; no new dependencies; docs note that
  the HTTP/web servers now support the Postgres backend.

### In scope

- SC-1..SC-7: move axum store calls off the tokio runtime (both servers, all
  backends), preserving RLS scoping, with a gated HTTP+PG proof.

### Out of scope

- Making the store trait async / adopting `tokio-postgres` (rejected — breaks
  the synchronous-store invariant).
- Connection pooling (still one guarded connection; F-003b's session-`SET`
  caveat is unchanged).
- Adding tenant isolation wiring to the `web` dashboard (admin view is
  unrestricted by design).
- Refactoring web.rs's pre-existing `store.lock().unwrap()` style beyond
  routing through the actor handle (which retires it anyway).

### Corner cases

- Actor jobs must be `Send + 'static` and return `Send`: the closure captures
  owned request params (RpcRequest/RecallReq/…) and returns `Send` results
  (RpcResponse/`Vec<Memory>`/StoreStats/topics). The `Store` is NOT captured —
  it is owned by the actor thread and lent to the job as `&Store`.
- The embedder is owned by the actor thread and lent as `Option<&dyn
  Embedder>`; blocking embed work (local fastembed CPU / cloud `ureq`) also
  runs on the actor thread, off the tokio worker.
- Actor serialization replaces the `Mutex`: one job runs at a time, so
  concurrency/correctness match today and the F-003b tenant-then-query
  invariant holds because both run in one job.
- If a job panics, the oneshot sender drops → the handler's `.await` gets an
  error → 500 (the actor loop should catch/continue, or the thread restarts;
  either way no server crash).

## Architecture / Context dependencies

- **`http_api.rs`** (F-001, `http-api` feature) and **`web.rs`** (`web`
  feature) are both `#[tokio::main]` axum servers that today hold
  `Arc<Mutex<Store>>` and call synchronous store methods directly on tokio
  workers. They switch to holding a `StoreHandle` (the actor's channel sender).
- **`icm-store` blocking `postgres` 0.19 client** wraps `tokio-postgres` with
  an internal runtime + `block_on`; invoked on **any tokio-managed thread**
  (async worker OR `spawn_blocking` pool — both have the runtime context
  entered) it panics ("runtime within a runtime"). The **actor thread is a
  plain `std::thread`** with no runtime context, so `block_on` works there.
- **Store trait stays synchronous** — the actor calls the same sync methods;
  only *where* they run changes. CLI/MCP-stdio paths (already sync, no tokio)
  are untouched.
- **F-003b** `Store::set_tenant` + RLS run inside the actor job, before the
  query, in the same job — the lock-split hazard remains avoided.
- **New module** `store_actor.rs` (gated on `http-api`/`web`) hosts the
  handle + thread; both servers construct it in their `run_*_server`.

## Environment requirements

- **No new dependencies** — `std::thread` + `std::sync::mpsc` +
  `tokio::sync::oneshot` (tokio already a dep of `http-api`/`web`).
- **Runtime impact:** store work moves to one dedicated OS thread (serialized
  as the `Mutex` was), freeing tokio workers; negligible cost.
- **Feature-gated:** changes compile only under `http-api` / `web`; the
  default binary's CLI/MCP paths are unaffected; the default (SQLite) build
  behaves identically.
- **Test PG:** reuse the docker `pgvector/pgvector:pg16` harness (host port
  5433); the new HTTP+PG test is gated on `ICM_POSTGRES_URL`.

## Definition of Done delta

Appended to the project contract's Definition of Done for F-003c:

1. `icm serve --http` (and `--web`) on the Postgres backend serve store
   operations without panicking; a gated test proves an HTTP store op returns
   200 against Postgres.
2. Default `cargo test --workspace` green; existing `http_api`/`web` SQLite
   tests unchanged (zero-regression).
3. `clippy -D warnings` on default and on `http-api,web,postgres,remote-store`;
   `fmt` clean; no new dependencies.
4. F-003b RLS scoping still holds over HTTP+PG (set_tenant + query share one
   actor job).
5. A store-job panic returns HTTP 500, not a crashed server.
6. Docs note that the HTTP/web servers support the Postgres backend.

## Handoff state requirements

- The production topology (thin clients → `icm serve --http` → Postgres,
  multi-tenant) is functional end-to-end; the F-003b demo that panicked now
  completes.
- Docs (`docs/postgres-backend.md` and/or `docs/remote-backend.md`) reflect
  that HTTP/web over Postgres is supported (with the F-003b non-superuser
  requirement unchanged).
- Observability: contained 500s on store-closure panics; no silent worker
  death.
- Delivery via B-workflow (local merge to `main`, push fork `origin`, no
  upstream PR).

## 7-gate impact map

- **G1 (code)** — `store_actor.rs` (handle + thread); actor-routed store ops in `http_api.rs` (6
  handlers) + `web.rs` (store-locking handlers); a small async helper.
- **G2 (TDD red-green)** — the HTTP+PG no-panic test is written failing first
  (reproduces the panic → 500/empty) against a live PG, then green after the
  fix.
- **G3 (verify)** — `ICM_POSTGRES_URL=… cargo test -p icm-cli --features
  "http-api,remote-store,postgres" http_pg_…`; default `cargo test`.
- **G4 (DoD)** — the 6 items above.
- **G5 (E2E)** — re-run the F-003b two-tenant demo over HTTP+PG: store ops
  succeed and isolation holds (non-superuser role).
- **G6 (review)** — Send/'static closure correctness; lock not split
  (set_tenant + query same closure); JoinError→500; uniform-backend; no
  sync-invariant violation.
- **G7 (handoff)** — features.md + docs updated; production HTTP+PG path
  usable.
