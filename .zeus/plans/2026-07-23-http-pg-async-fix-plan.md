# F-003c Implementation Plan — HTTP/web + Postgres async-blocking fix (store-thread actor)

## Header

- **Goal:** Stop `icm serve --http` and `icm serve --web` panicking ("Cannot
  start a runtime from within a runtime") on Postgres by running every axum
  handler's synchronous store work on a **dedicated store-owning
  `std::thread` (actor)** — a thread tokio does not manage, so the blocking
  postgres client's `block_on` works. Store trait stays synchronous.
- **Architecture:** a `StoreHandle` (cloneable channel sender) fronts one
  actor thread that owns the `Store` (+ embedder). Handlers submit a
  `FnOnce(&Store, Option<&dyn Embedder>) -> T` job and `.await` its result via
  a oneshot. The actor processes one job at a time (replacing the `Mutex` as
  the serialization point) and catches job panics so it never dies.
- **Tech Stack:** Rust (async/tokio + `std::thread`/`mpsc`), HTTP API (axum).
- **Feature tag:** F-003c
- **Spec:** `.zeus/specs/2026-07-23-http-pg-async-fix-design.md`

> **Why not `spawn_blocking`** (the original plan): tokio enters the runtime
> context on its blocking-pool threads too (probed: `Handle::current()`
> succeeds inside `spawn_blocking`), so the postgres client's `block_on`
> still panics there. Only a non-tokio `std::thread` escapes it. User-approved
> pivot.

## File Map

| File | Created/Modified | Responsibility |
|---|---|---|
| `crates/icm-cli/src/store_actor.rs` | **Created** | `StoreHandle` (clone) + `spawn(store, embedder)` (owns them on one `std::thread`, loops over an `mpsc` of jobs, `catch_unwind` per job) + `async run<T,F>(f)` (submit job, await oneshot). Gated on `http-api`/`web`. Unit-tested on SQLite. |
| `crates/icm-cli/src/main.rs` | Modified | `#[cfg(any(feature="http-api", feature="web"))] mod store_actor;`. |
| `crates/icm-cli/src/http_api.rs` | Modified | `AppState.store: StoreHandle` (drop the separate `embedder` field — the actor owns it); `run_http_server` spawns the actor; `blocking_store` helper now calls `handle.run(...)` (set_tenant + `f` in one job); the 6 handlers keep their closures. Gated PG no-panic test. |
| `crates/icm-cli/src/web.rs` | Modified | `AppState.store: StoreHandle`; `run_web_server` spawns the actor; add `blocking_web` helper calling `handle.run(...)` (no tenant); route the ~14 `api_*` store handlers through it (retiring `store.lock().unwrap()`); extract `web_router(state)` for tests. Gated PG smoke test. |
| `docs/postgres-backend.md` | Modified | Note HTTP/web support the Postgres backend (store ops run on a dedicated thread). |
| `.zeus/features.md` | Modified | F-003c → `done` (G7). |

`store_actor.rs` ~110 lines (util/helper). `http_api.rs`/`web.rs` net changes
are contained (helper + setup + field type). See §10.

## Architect Risk Analysis

### Rust (async / tokio / threads) lens

- **Restate:** one `std::thread` owns the `Store`; handlers hand it jobs over
  a channel and await results. The thread has no tokio runtime context, so the
  postgres client's `block_on` works.
- **Risk — actor thread death on job panic (critical):** if a job panics, the
  thread would unwind and die → every later request's oneshot never resolves →
  hung handlers. Mitigation: wrap each job in
  `std::panic::catch_unwind(AssertUnwindSafe(...))`; on panic the job's oneshot
  sender drops → that handler's `.await` returns `Err` → 500, and the actor
  loops on to the next job. A dedicated regression test asserts "a panicking
  job errors, and the next job still succeeds".
- **Risk — handler hang if handle dropped / channel closed:** `run()` maps a
  failed `tx.send` (actor gone) and a dropped oneshot to an error → 500, never
  an infinite await.
- **Risk — `Send + 'static` job bounds:** the job closure captures owned
  params + the oneshot sender and returns `Send`; the `Store`/embedder are NOT
  captured (owned by the thread, lent as `&Store`/`Option<&dyn Embedder>`).
- **Risk — F-003b lock-split:** `set_tenant` + the query run in ONE job on the
  single actor thread → atomic, un-interleaved. `blocking_store` builds that
  one job.
- **Risk — store lifetime:** the actor thread lives for the server's lifetime
  (the handle lives in `AppState`, cloned per request; the thread runs until
  all senders drop). No graceful-shutdown join needed for a long-running
  server (documented; process exit reclaims it).

### HTTP API lens

- **Restate:** `AppState.store` changes type (`Arc<Mutex<Store>>` →
  `StoreHandle`); handler bodies keep their `|store, emb| …` closures via the
  `blocking_store`/`blocking_web` helpers.
- **Risk — response parity:** results/status must match today on SQLite (the
  job computes the same value; formatting unchanged). Mitigation: existing
  SQLite `http_api`/`web` tests stay green.
- **Risk — web `.unwrap()` retirement:** routing web handlers through the
  actor replaces `store.lock().unwrap()` with a graceful 500 — an improvement.
- **Question (resolved):** actor for all backends (uniform); `web` gets no
  tenant wiring (admin = unset = unrestricted).

*User: confirm this analysis (esp. per-job `catch_unwind` so the actor
survives panics, and set_tenant+query in one job) — covered by the approval
gate.*

## Tasks

Provision PG once before T2 (with the F-003b non-superuser `icm_app` role for
the T5 demo):

```bash
docker run -d --name icm-pg -e POSTGRES_USER=icm -e POSTGRES_PASSWORD=icm \
  -e POSTGRES_DB=icm -p 5433:5432 pgvector/pgvector:pg16
docker exec icm-pg psql -U icm -d icm -c "CREATE EXTENSION IF NOT EXISTS vector; \
  CREATE ROLE icm_app LOGIN PASSWORD 'icm_app' NOSUPERUSER; GRANT ALL ON SCHEMA public TO icm_app;"
export ICM_POSTGRES_URL="postgres://icm:icm@localhost:5433/icm"
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
```

### T1 — store_actor module (the mechanism, SQLite-testable)

1. **Failing test** (in `store_actor.rs`, no PG needed): the actor runs jobs,
   survives a panicking job, and stays usable.
   ```rust
   #[tokio::test(flavor = "multi_thread")]
   async fn actor_runs_survives_panic() {
       let s = Store::in_memory().unwrap();
       let h = StoreHandle::spawn(s, None);
       // normal job returns a value
       assert_eq!(h.run(|st, _| st.count().unwrap()).await.unwrap(), 0);
       // a panicking job errors, does NOT kill the actor
       assert!(h.run(|_, _| panic!("boom")).await.is_err());
       // actor still serves the next job
       assert_eq!(h.run(|st, _| st.count().unwrap()).await.unwrap(), 0);
   }
   ```
2. **Confirm fail:** `cargo test -p icm-cli --features http-api --bin icm actor_runs_survives_panic` → module/type absent (compile error).
3. **Minimal impl:** create `store_actor.rs`: `type Job = Box<dyn FnOnce(&Store, Option<&dyn Embedder>) + Send>`; `StoreHandle { tx: std::sync::mpsc::Sender<Job> }` (`#[derive(Clone)]`); `spawn(store, embedder)` → `std::thread::spawn` loop `while let Ok(job) = rx.recv() { let _ = std::panic::catch_unwind(AssertUnwindSafe(|| job(&store, embedder.as_deref().map(|e| e as &dyn Embedder)))); }`; `async fn run<T: Send+'static, F: FnOnce(&Store, Option<&dyn Embedder>)->T + Send+'static>(&self, f) -> Result<T, ActorError>` → make a `tokio::sync::oneshot`, box a job that `let _ = otx.send(f(store, emb));`, `tx.send(job).map_err(|_| Down)?`, `orx.await.map_err(|_| Down)`. Declare `mod store_actor;` in main.rs.
4. **Confirm pass / commit:** `feat(cli): store-actor thread — serve the sync Store off any tokio runtime`.

### T2 — http_api on the actor (fixes the PG panic)

1. **Failing test** (gated PG, `rpc_tests`): the existing
   `http_pg_store_op_no_runtime_panic` (a `/rpc` store+count over Postgres →
   200). It currently panics (the demo bug).
2. **Confirm fail:** `ICM_POSTGRES_URL=… cargo test -p icm-cli --features "http-api,remote-store,postgres" http_pg_store_op_no_runtime_panic -- --test-threads=1` → runtime-nesting panic.
3. **Minimal impl:** `AppState.store: StoreHandle` (remove the `embedder`
   field — the actor owns it); `run_http_server` builds
   `StoreHandle::spawn(store, embedder)`; rewrite `blocking_store` to
   `self`-less `state.store.run(move |store, emb| { if let Some(t)=tenant { store.set_tenant(Some(&t)).map_err(..)?; } Ok(f(store, emb)) }).await` mapping `ActorError`/tenant-err → 500(format). The 6 handler closures stay as written. `pg_app_state` test helper: connect on a `std::thread` (off-runtime) then `StoreHandle::spawn`.
4. **Confirm pass / commit:** green + existing SQLite `rpc_tests`/`cache_tests` green → `feat(cli): serve http_api store ops via the store actor (fixes PG panic)`.

### T3 — web dashboard on the actor

1. **Failing test** (gated PG, `web` tests): `web_pg_stats_no_runtime_panic` —
   `/api/stats` over Postgres → 200. (Extract `web_router(state)` first if the
   dashboard has no testable router builder; no behavior change.)
2. **Confirm fail:** `ICM_POSTGRES_URL=… cargo test -p icm-cli --features "web,postgres" web_pg_stats_no_runtime_panic -- --test-threads=1` → panic.
3. **Minimal impl:** `AppState.store: StoreHandle`; `run_web_server` spawns the
   actor; add `blocking_web<T,F>(state, f) -> Result<T, Response>` (no tenant)
   calling `state.store.run(f)`; route the ~14 store `api_*` handlers
   (api_stats, api_topics, api_topic_detail, api_topic_health,
   api_topic_consolidate, api_memories, api_memories_search, api_memory_delete,
   api_health_all, api_decay, api_prune, api_memoirs, api_memoir_detail)
   through it, retiring `store.lock().unwrap()`.
4. **Confirm pass / commit:** green + existing SQLite web tests green →
   `feat(cli): serve web dashboard store ops via the store actor (PG support)`.

### T4 — Docs

1. **Verification:** `grep -qi "dedicated.*thread\|store actor\|HTTP.*Postgres\|支持.*postgres" docs/postgres-backend.md`.
2. **Implement:** note in `docs/postgres-backend.md` that `icm serve --http`
   and `--web` support Postgres (store ops run on a dedicated store thread);
   F-003b non-superuser requirement unchanged.
3. **Commit:** `docs(store): HTTP/web servers support the Postgres backend`.

### T5 — Full gate + zero-regression + demo re-run

1. **Verification:**
   - `cargo fmt --all -- --check`
   - `cargo clippy --workspace --all-targets -- -D warnings` (default)
   - `cargo clippy -p icm-cli --features "http-api,web,postgres,remote-store" --all-targets -- -D warnings`
   - `env -u ICM_POSTGRES_URL cargo test --workspace` → green; SQLite HTTP/web/cache tests unchanged.
   - `ICM_POSTGRES_URL=… cargo test -p icm-cli --features "http-api,web,remote-store,postgres" -- --test-threads=1` → actor + both gated PG tests green.
   - `cargo build -p icm-cli --no-default-features --features "backend-sqlite,postgres,http-api,web,remote-store"` compiles.
   - **Demo re-run (G5):** `icm serve --http` on PG as `icm_app`; two tokens store+read → succeed (no panic), each tenant sees only its own.
2. **Implement:** fix any gate issue.
3. **Commit:** `chore(f-003c): verify gates + zero-regression`.

## Test Plan

- **Unit (no PG):** `actor_runs_survives_panic` (T1) — job runs, panic
  contained + actor survives; covers the critical robustness risk on SQLite.
- **Integration (gated PG):** `http_pg_store_op_no_runtime_panic` (T2) — /rpc
  store+count over PG → 200; `web_pg_stats_no_runtime_panic` (T3) — /api/stats
  over PG → 200.
- **E2E (gated PG, scripted):** F-003b two-tenant demo over `icm serve --http`
  + PG (`icm_app`): both tenants store, each sees only its own (T5).
- **Regression:** `env -u ICM_POSTGRES_URL cargo test --workspace` — existing
  SQLite `http_api` (rpc_tests, cache_tests) + `web` tests pass unchanged (the
  actor is transparent to the SQLite path); default build behavior identical.

## Security Review

- **Static:** `cargo clippy --features "http-api,web,postgres,remote-store" -- -D warnings`; `cargo audit` (no new deps — std thread/mpsc + tokio oneshot).
- **Threat model:** changes *where* store code runs, not the attack surface.
  - **Auth intact:** middleware runs before handlers; unknown/missing token →
    401 unchanged.
  - **RLS scoping preserved:** set_tenant + query in one actor job (F-003b),
    isolation not weakened; the demo re-run confirms.
  - **DoS/robustness — improved:** per-job `catch_unwind` means one panic
    can't kill the actor / hang the server; `run` errors instead of hanging on
    a dead actor.
  - **No secrets in logs:** unchanged (tenant only, never tokens).
  - **Injection:** none new (no new SQL; `set_config` still bound).
- **Specific checks:** input validation unchanged; no SSRF/path traversal;
  single actor thread bounds store concurrency.

## Logic Review Checkpoints

- **CP-A (after T1):** actor loop `catch_unwind`s each job (survives panics);
  `run` never hangs (send-fail + dropped-oneshot → error); job is `Send +
  'static`; `Store`/embedder owned by the thread, not captured.
- **CP-B (after T2):** `AppState.store` is `StoreHandle`; no `Arc<Mutex<Store>>`
  left in http_api; set_tenant + query in ONE job (F-003b); no `MutexGuard`
  anywhere; SQLite `rpc_tests`/`cache_tests` byte-identical; gated PG test 200.
- **CP-C (after T3):** every web store handler routed through the actor; no
  `store.lock().unwrap()` remains; no tenant wiring added to web; SQLite web
  tests unchanged.
- **CP-D (gate/demo):** HTTP+PG store op 200 (no panic); two-tenant demo
  isolation holds; default build zero-regression.

## G4 contract delta

Appended to the project contract's Definition of Done after this plan:

1. `ICM_POSTGRES_URL=… cargo test -p icm-cli --features "http-api,web,remote-store,postgres" -- --test-threads=1` — actor test + HTTP `/rpc` + web `/api/stats` return 200 over Postgres (no runtime-nesting panic).
2. `env -u ICM_POSTGRES_URL cargo test --workspace` green; SQLite HTTP/web unchanged.
3. `clippy -D warnings` on default and `http-api,web,postgres,remote-store`; `fmt` clean; no new deps.
4. F-003b RLS scoping holds over HTTP+PG (set_tenant + query one actor job).
5. A panicking store job → HTTP 500, actor survives, server stays up.
6. `docs/postgres-backend.md` states HTTP/web support Postgres.

## Logic Completeness Manifest

**Every requirement in the linked spec MUST be implemented in full.
Authorized simplifications: (none).**

Scope honesty (spec-declared, not a cut): the `web` dashboard gets the actor
fix but **no tenant isolation wiring** (admin = unset = unrestricted by
design). RLS isolation over HTTP still requires the Postgres backend + a
non-superuser role (F-003b), unchanged.

### Spec Coverage Matrix

| SC-ID | Capability | Implementing task(s) | Verification command |
|---|---|---|---|
| SC-1 | Store-actor thread + http_api handlers routed through it | T1, T2 | `cargo test -p icm-cli --features http-api --bin icm actor_runs_survives_panic` + `ICM_POSTGRES_URL=… cargo test -p icm-cli --features "http-api,remote-store,postgres" http_pg_store_op_no_runtime_panic -- --test-threads=1` |
| SC-2 | F-003b set_tenant+query in one actor job | T2 | same PG test + CP-B review + demo isolation (T5) |
| SC-3 | web.rs handlers routed through the actor (PG works) | T3 | `ICM_POSTGRES_URL=… cargo test -p icm-cli --features "web,postgres" web_pg_stats_no_runtime_panic -- --test-threads=1` |
| SC-4 | Uniform all backends; SQLite tests green | T2, T3, T5 | `env -u ICM_POSTGRES_URL cargo test --workspace` |
| SC-5 | Job panic → 500, actor survives (no hang/crash) | T1 | `actor_runs_survives_panic` |
| SC-6 | Gated HTTP+PG no-panic proof | T2 | `http_pg_store_op_no_runtime_panic` (gated) |
| SC-7 | Zero-regression + hygiene + docs | T4, T5 | `env -u ICM_POSTGRES_URL cargo test --workspace` + `cargo clippy … -D warnings` + `grep -qi postgres docs/postgres-backend.md` |

All SC-1..SC-7 mapped. Manual coverage check (no `scripts/check-spec-coverage.sh`) — declared degraded/manual, no orphans.

## File Size Constraints

| File | Projected | Threshold | Flag |
|---|---|---|---|
| `crates/icm-cli/src/store_actor.rs` | ~110 lines (new: handle + thread + run + test) | Utility/helper | OK |
| `crates/icm-cli/src/http_api.rs` | ~1300 lines (helper reworked, field type, setup) | HTTP controller (relaxed) | OK (relaxed) |
| `crates/icm-cli/src/web.rs` | ~700 lines (+helper, +router builder, handler routing) | HTTP controller (relaxed) | OK (relaxed) |
| `crates/icm-cli/src/main.rs` | +1 line (mod decl) | ~10k (relaxed, entrypoint) | OK (relaxed) |
| `docs/postgres-backend.md` | ~215 lines | Docs (relaxed) | OK (relaxed) |
| `.zeus/features.md` | +1 line | Docs (relaxed) | OK (relaxed) |

No `OVER` rows.

**User-approved:** 2026-07-23 by rainhan@coupert.com (revised: store-thread actor)
