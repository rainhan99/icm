# F-003a — Postgres backend: full subsystem parity + PG test harness

Extend the Postgres backend so a central Postgres node is a **complete
replacement for SQLite** across all five subsystems, and add a PostgreSQL
integration-test harness. This is the prerequisite for F-003b (row-level
tenant isolation + RLS).

Predecessors: F-001 (three-tier shared memory), F-002 (code graph), F-003
phase-1 (supersession + auth scaffold) — all done.

## Goal / Scope

Today `PostgresStore` (crates/icm-store/src/postgres.rs, pgvector) implements
`MemoryStore` fully but **stubs the other four trait surfaces** —
`FactsStore` (~6), `MemoirStore` (~25), `FeedbackStore` (~6),
`TranscriptStore` (~9) — returning `IcmError::Unsupported`. On a Postgres
central node, facts / memoir / feedback / transcript operations therefore
fail. F-003a implements all four on Postgres with parity to the SQLite
backend, adds PG-native full-text search, pre-seeds (unused) tenant columns
to pave F-003b, and stands up an integration-test harness.

**Honest boundary:** F-003a delivers full *functional* parity. It does
**not** add tenant data isolation — a Postgres node with F-003a still
serves one shared dataset (exactly like SQLite today). Isolation is F-003b.

### Scope Checklist

- **SC-1** — `FactsStore` on Postgres: all trait methods, including exact
  `(entity, key, value)` versioning + history, matching the SQLite backend's
  observable semantics. Replaces the `Unsupported` stub.
- **SC-2** — `MemoirStore` on Postgres: all trait methods, including concept
  CRUD, concept links, and graph traversal (`get_neighbors`,
  `get_neighborhood`) via SQL (recursive CTE or bounded iterative). Replaces
  the `Unsupported` stub.
- **SC-3** — `FeedbackStore` on Postgres: all trait methods, with full-text
  search over feedback (the SQLite backend uses FTS5). Replaces the stub.
- **SC-4** — `TranscriptStore` on Postgres: all trait methods — session /
  message store, verbatim replay, and message search (SQLite uses FTS5).
  Replaces the stub.
- **SC-5** — Idempotent PG schema: extend `init_schema` with
  `CREATE TABLE IF NOT EXISTS` for every new subsystem table (facts, memoir,
  concepts, concept_links, feedback, sessions, messages), re-runnable against
  an existing DB without error.
- **SC-6** — PG-native full-text search: `tsvector` columns +
  `plainto_tsquery`/`to_tsquery` + `ts_rank` ranking + GIN indexes for the
  searchable subsystems (memoir concepts, feedback, transcript messages).
- **SC-7** — Pre-seed a **nullable `tenant`** column on every **new** table
  (facts, memoir, concepts, concept_links, feedback, sessions, messages),
  left unpopulated and unfiltered by F-003a. The existing `memories` table's
  tenant column and ALL tenant filtering / RLS are explicitly deferred to
  F-003b.
- **SC-8** — PG integration-test harness: tests gated on `ICM_POSTGRES_URL`
  (skip cleanly when unset, so the default `cargo test` is unaffected);
  per-subsystem round-trip coverage (facts versioning, memoir links +
  neighborhood, feedback search, transcript store/search/replay) against a
  live PG. Documented local `docker` workflow.
- **SC-9** — Optional CI job: a Postgres+pgvector service container running
  the gated PG tests. May be opt-in / non-blocking (the user asked for CI to
  be optional; local docker is the primary path).
- **SC-10** — Zero-regression + hygiene: default (SQLite) build byte-identical
  behavior (`postgres` is opt-in, non-default); `clippy -D warnings` on
  `--features postgres`; `docs/postgres-backend.md` updated (full-subsystem
  support, test how-to, pre-seeded-tenant note + F-003b pointer); the
  "Unsupported subsystems" note in `postgres.rs` corrected.

### In scope

- SC-1..SC-10: functional parity of all four remaining subsystems on
  Postgres + PG-native FTS + tenant-column pre-seed + integration-test
  harness.

### Out of scope (explicitly deferred / limitations)

- **Tenant data isolation** (row-level tenant filtering + Postgres RLS +
  `memories` tenant column) → **F-003b**.
- **OpenSearch** backend subsystem parity (separate backend, separate work).
- Node-local bookkeeping beyond what the memory backend already provides
  (hook events, code areas, pending extraction, pattern mining) — unchanged.
- Cross-backend data migration tooling (SQLite → PG dump/load).

### Corner cases

- Idempotent migration: re-running `init_schema` on a populated DB (ADD
  COLUMN IF NOT EXISTS, CREATE INDEX IF NOT EXISTS) must not error.
- Facts versioning: superseding a fact key must preserve prior versions for
  `history`, matching SQLite.
- Memoir graph: cycles in concept links; `get_neighborhood` depth bound;
  self-links; missing endpoints.
- FTS ranking differs from FTS5 bm25 — tests assert *relevant hit membership
  / ordering by rank*, not identical scores.
- Empty results, non-existent ids, and concurrent writers behave like SQLite
  (typed errors, no panics).
- pgvector dims: concept/embedding vectors (if any) must respect the store's
  configured dimensions.

## Architecture / Context dependencies

- **Mirror the SQLite backend** (`crates/icm-store/src/store.rs` +
  `schema.rs`) for each subsystem's observable semantics; translate FTS5
  MATCH → `tsvector @@ tsquery` and rowid tricks → PG equivalents.
- **Traits** live in `icm-core` (`facts_store.rs`, `memoir_store.rs`,
  `feedback_store.rs`, `transcript_store.rs`); implement each for
  `PostgresStore`, replacing the `Unsupported` stubs at the bottom of
  `postgres.rs`.
- **Dispatch is already wired**: the `Store` enum (`backend.rs`) already
  routes these traits to `PostgresStore`, so once implemented they light up
  under `ICM_DB_BACKEND=postgres` with no dispatch changes.
- **Schema**: extend the existing `init_schema(client, dims)` (postgres.rs)
  — same idempotent `CREATE ... IF NOT EXISTS` pattern already used for
  `memories` / `icm_metadata` / `pending_extractions` / `code_areas` /
  `hook_events`.
- **Sync store invariant** holds: the blocking `postgres` client (v0.19) +
  `pgvector` (v0.4) are already dependencies; no async introduced.

## Environment requirements

- **No new runtime dependencies**: `postgres` + `pgvector` are already
  present; `tsvector`/GIN are built into PostgreSQL; `CREATE EXTENSION vector`
  is already issued by `init_schema`. `pg_trgm` is **not** required.
- **No new test dependencies**: the harness is env-gated (reads
  `ICM_POSTGRES_URL`), so no `testcontainers` / Docker-in-test dependency.
- **Runtime**: `postgres` is an **opt-in, non-default** feature; the default
  SQLite build and its hot path are untouched.
- **CI (optional)**: an added job may run a `pgvector/pgvector` (or
  `postgres` + extension) service container and export `ICM_POSTGRES_URL`;
  may be non-blocking.
- **Version pins**: no changes; relies on already-pinned `postgres 0.19` /
  `pgvector 0.4` and the `vector` extension.

## Definition of Done delta

Appended to the project contract's Definition of Done for F-003a:

1. `cargo build --no-default-features --features postgres` compiles with
   `FactsStore`/`MemoirStore`/`FeedbackStore`/`TranscriptStore` fully
   implemented for `PostgresStore` (no `Unsupported` for these four).
2. `clippy -D warnings` passes on `--features postgres`.
3. Gated PG integration tests pass against a live PG:
   `ICM_POSTGRES_URL=postgres://… cargo test -p icm-store --features postgres`
   — each subsystem round-trips (facts versioning/history, memoir
   links+neighborhood, feedback search, transcript store/search/replay).
4. With `ICM_POSTGRES_URL` unset, those tests **skip** and the default
   `cargo test --workspace` stays green (zero-regression on SQLite).
5. All new PG tables carry a nullable `tenant` column (schema-introspection
   assertion); `memories` intentionally unchanged (F-003b).
6. `init_schema` is idempotent: running it twice against the same DB is a
   no-op, not an error.
7. `docs/postgres-backend.md` documents full-subsystem support, the local
   docker test workflow, and the pre-seeded-tenant / F-003b boundary.

## Handoff state requirements

- **F-003b unblocked**: schema pre-seeded with tenant columns on new tables;
  F-003b adds the `memories` tenant column + populates/filters + RLS
  policies + `SET LOCAL app.tenant`, as a policy-layer change.
- `docs/postgres-backend.md` reflects the new reality (was: "Postgres covers
  only core memory"); the `postgres.rs` header note is corrected.
- `.zeus/features.md`: F-003a → done; F-003b remains planned with the
  dependency noted.
- Observability: all PG paths map errors to typed `IcmError` (no
  `unwrap`/`expect`); connection/setup failures are actionable.
- Delivery via B-workflow (local merge to `main`, push fork `origin`, no
  upstream PR).

## 7-gate impact map

- **G1 (code)** — 4 trait impls (~46 methods) on `PostgresStore` + schema
  extension in `init_schema`.
- **G2 (TDD red-green)** — each subsystem's PG round-trip test written
  failing first (against a live PG), then green; SQLite parity tests are the
  behavioral reference.
- **G3 (verification command)** — `ICM_POSTGRES_URL=… cargo test -p icm-store
  --features postgres pg_…` per subsystem; `cargo build --features postgres`.
- **G4 (DoD)** — the 7 DoD items above; new contract items appended.
- **G5 (E2E)** — a live-PG E2E: point `ICM_DB_BACKEND=postgres` + a running
  server, exercise facts/memoir/feedback/transcript through the store enum.
- **G6 (review)** — parity review against SQLite semantics; SQL-injection
  check (parameterized queries only); idempotent-migration review; no-panic
  review.
- **G7 (handoff)** — features.md + docs updated; F-003b prerequisites
  recorded.
