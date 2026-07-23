# F-003a Implementation Plan — Postgres full subsystem parity + PG test harness

## Header

- **Goal:** Implement `FactsStore`, `MemoirStore`, `FeedbackStore`,
  `TranscriptStore` (~46 methods) on `PostgresStore`, replacing the
  `Unsupported` stubs, so a central Postgres node is a complete SQLite
  replacement. Add PG-native full-text search (tsvector/GIN), pre-seed
  nullable `tenant` columns on new tables, and stand up an env-gated PG
  integration-test harness.
- **Architecture:** Extend `crates/icm-store/src/postgres.rs` (blocking
  `postgres` 0.19 + `pgvector` 0.4, already deps). Schema grows in the
  existing `init_schema`. The `Store` enum (`backend.rs`) already dispatches
  these four traits to `PostgresStore`, so no dispatch changes are needed —
  implementing the traits lights them up under `ICM_DB_BACKEND=postgres`.
- **Tech Stack:** Rust (sync store), PostgreSQL/SQL (pgvector, tsvector/GIN,
  recursive CTE).
- **Feature tag:** F-003a
- **Spec:** `.zeus/specs/2026-07-22-postgres-full-subsystem-design.md`

## File Map

| File | Created/Modified | Responsibility |
|---|---|---|
| `crates/icm-store/src/postgres.rs` | Modified | Extend `init_schema` (7 tables + tsvector generated columns + GIN indexes + nullable `tenant` columns + facts partial-unique index); implement `FactsStore`/`MemoirStore`/`FeedbackStore`/`TranscriptStore` (replace `Unsupported` stubs); PG FTS + recursive-CTE graph helpers; gated `#[cfg(all(test, feature="postgres"))] mod pg_tests`; correct the "Unsupported subsystems" header note. |
| `docs/postgres-backend.md` | Modified | Document full-subsystem support, the local `docker` (pgvector) test workflow, and the pre-seeded-`tenant` / F-003b boundary. |
| `.github/workflows/ci.yml` | Modified | Add an opt-in Postgres+pgvector **service-container** job that exports `ICM_POSTGRES_URL` and runs the gated PG tests (SC-9; may be non-blocking). |
| `.zeus/features.md` | Modified | F-003a → `done` (G7 handoff). |

No new source files: the SQLite backend keeps all subsystems in one
`store.rs` (~4000 lines); mirroring that, PG subsystems stay in `postgres.rs`
(projected ~2800 lines — under `store.rs`, consistent with the repo's
"one persistence file per backend" convention). See §10.

## Architect Risk Analysis

### Rust (sync store) lens

- **Restate:** Implement 4 traits (~46 methods) on `PostgresStore`, mirroring
  SQLite semantics, over a blocking client, with typed `IcmError` and no
  `unwrap`/`expect`.
- **Risk — row extraction panics:** the `postgres` crate's `Row::get` **panics**
  on a column/type mismatch. Must use `Row::try_get(...)` everywhere and map
  the error via the existing `db_err` helper — never `row.get`. A single
  `row.get` is a production panic path (violates the no-panic invariant).
- **Risk — interior mutability:** `postgres::Client` query methods take
  `&mut self`, but the store traits take `&self`. Reuse the **exact pattern
  the existing `MemoryStore for PostgresStore` uses** (a `Mutex<Client>` /
  interior cell). New methods must lock the same guard; lock-poisoning maps
  to a typed error, not a panic.
- **Risk — connection scaling:** one guarded connection serializes all access.
  At 200 users a central node wants pooling — but that is the server's
  concern and matches the current PG `MemoryStore` design. Out of scope;
  recorded as a known limitation.
- **Question (resolved):** timestamps stored as `TEXT` RFC3339 (parity with
  SQLite + how `Fact`/`Concept`/`Session` serialize), not `timestamptz`.

### PostgreSQL / SQL lens

- **Restate:** 7 new tables, tsvector+GIN FTS, recursive-CTE neighborhood,
  facts partial-unique index, nullable tenant columns, idempotent migration.
- **Risk — FTS semantics differ from FTS5:** use text-search config
  **`simple`** (no stemming/stopwords) as the closest match to FTS5's
  tokenizer for mixed/multilingual content. `ts_rank` ordering ≠ bm25, so
  tests assert **hit membership + relative rank order**, never absolute
  scores.
- **Risk — set_fact race:** "supersede active row, then insert new" must be a
  single **transaction** (`BEGIN; UPDATE … SET superseded_at; INSERT; COMMIT`)
  or two concurrent writers can both insert, violating the partial-unique
  index `WHERE superseded_at IS NULL`. Wrap it; on unique-violation the txn
  aborts cleanly to a typed error.
- **Risk — graph direction/cycles:** `get_neighborhood(depth)` via
  `WITH RECURSIVE` must (a) dedup via `UNION`, (b) bound recursion by a level
  counter, (c) match SQLite's link-direction semantics exactly. Mitigation:
  read the SQLite `get_neighborhood`/`get_neighbors` impl first and mirror
  direction + dedup precisely (T8 step 0).
- **Risk — tsvector maintenance:** use `GENERATED ALWAYS AS (to_tsvector(
  'simple', coalesce(col,'') || ' ' || …)) STORED` columns rather than
  triggers — declarative, idempotent, no trigger drift.
- **Risk — idempotency:** rely on `CREATE TABLE IF NOT EXISTS`,
  `ADD COLUMN IF NOT EXISTS` (PG ≥ 9.6), `CREATE INDEX IF NOT EXISTS`; running
  `init_schema` twice must be a no-op.
- **Question (resolved by user):** nullable `tenant` on **new tables only**;
  `memories` untouched (F-003b).

*User: please confirm this risk analysis (esp. `simple` FTS config, TEXT
timestamps, transactional `set_fact`) — it is embedded as this section and
covered by the plan approval gate.*

## Tasks

Execution first provisions an ephemeral PG (once, before T1):

```bash
docker run -d --name icm-pg \
  -e POSTGRES_USER=icm -e POSTGRES_PASSWORD=icm -e POSTGRES_DB=icm \
  -p 5432:5432 pgvector/pgvector:pg16
export ICM_POSTGRES_URL="postgres://icm:icm@localhost:5432/icm"
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
```

Every task's verification runs with `--features postgres` and that env set.

### T0 — Reconnaissance (no code)

Read the existing `MemoryStore for PostgresStore` impl + `PostgresStore`
struct in `postgres.rs` to capture: the interior-mutability pattern (how
`&self` reaches `&mut Client`), the `db_err` mapping helper, timestamp
format, and the `Fact`/`Concept`/`ConceptLink`/`Feedback`/`Session`/`Message`
field↔column serialization used by SQLite (`store.rs`). No commit.

### T1 — Schema extension (7 tables + tsvector + GIN + tenant + partial-unique)

1. **Failing test** (`pg_tests`): assert the schema exists after connect.
   ```rust
   #[test]
   fn pg_schema_has_all_subsystem_tables_and_tenant() {
       let Some(s) = pg_test_store() else { return }; // skip if no URL
       let cols = s.debug_table_columns("facts").unwrap();
       assert!(cols.iter().any(|c| c == "tenant"), "facts.tenant pre-seeded");
       for t in ["memoirs","concepts","concept_links","feedback","sessions","messages","facts"] {
           assert!(s.debug_table_exists(t).unwrap(), "missing table {t}");
       }
       // idempotency: second init is a no-op
       assert!(s.debug_reinit_schema().is_ok());
   }
   ```
   (`pg_test_store`, `debug_*` helpers land in T2; T1's test is written now and
   fails to compile → red. If preferred, T1 and T2 land together — see step 2.)
2. **Confirm fail:** `cargo test -p icm-store --features postgres pg_schema_has_all` → compile error (helpers absent) or assertion failure (tables absent).
3. **Minimal impl:** extend `init_schema` with, for each subsystem, idempotent DDL mirroring `schema.rs`, translated to PG:
   - `memoirs`, `concepts` (FK→memoirs ON DELETE CASCADE, `UNIQUE(memoir_id,name)`), `concept_links` (FK→concepts, `UNIQUE(source,target,relation)`, `CHECK(source<>target)`), `feedback`, `facts` (+ `CREATE UNIQUE INDEX IF NOT EXISTS … ON facts(entity,key) WHERE superseded_at IS NULL`), `sessions`, `messages` (FK→sessions ON DELETE CASCADE).
   - Timestamps `TEXT`; JSON columns (`labels`, `source_memory_ids`, `metadata`) `TEXT`.
   - tsvector **generated** columns + GIN on `concepts` (name+definition+labels), `feedback` (topic+context+predicted+corrected+reason), `messages` (content): `ADD COLUMN IF NOT EXISTS tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', coalesce(...,'')|| ' ' ||…)) STORED;` + `CREATE INDEX IF NOT EXISTS … USING GIN(tsv)`.
   - `ALTER TABLE … ADD COLUMN IF NOT EXISTS tenant TEXT` on all 7 new tables.
   - Matching secondary indexes (idx_concepts_memoir, idx_facts_entity_key, idx_messages_session, …).
4. **Confirm pass / commit:** `cargo test -p icm-store --features postgres pg_schema_has_all` green → `feat(store): PG schema for facts/memoir/feedback/transcript (+ tsvector/GIN, nullable tenant)`.

### T2 — Gated PG test harness

1. **Failing test:** a smoke test proving the harness connects and cleans state.
   ```rust
   #[test]
   fn pg_harness_connects_and_resets() {
       let Some(s) = pg_test_store() else { return };
       s.debug_truncate_all().unwrap();
       assert_eq!(s.count().unwrap(), 0);
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres pg_harness` → helper absent.
3. **Minimal impl:** in `#[cfg(all(test, feature="postgres"))] mod pg_tests`, add:
   - `fn pg_test_store() -> Option<PostgresStore>`: returns `None` when
     `ICM_POSTGRES_URL` is unset (test **skips** cleanly → default `cargo test`
     unaffected, SC-8); else connects and `debug_truncate_all()` for isolation.
   - `#[cfg(test)]`-only `debug_*` inspection helpers on `PostgresStore`
     (`debug_table_exists`, `debug_table_columns`, `debug_truncate_all`,
     `debug_reinit_schema`) — parameterized queries against `information_schema`.
4. **Confirm pass / commit:** green → `test(store): env-gated PG integration harness (skips without ICM_POSTGRES_URL)`.

### T3 — FactsStore on Postgres (6 methods)

1. **Failing test:** versioning + history round-trip.
   ```rust
   #[test]
   fn pg_facts_versioning_and_history() {
       let Some(s) = pg_test_store() else { return };
       s.set_fact("user","editor","vim","t").unwrap();
       s.set_fact("user","editor","emacs","t").unwrap();       // supersede
       assert_eq!(s.get_fact("user","editor").unwrap().unwrap().value, "emacs");
       assert_eq!(s.history("user","editor").unwrap().len(), 2); // newest first
       assert_eq!(s.set_fact("user","editor","emacs","t2").unwrap(),
                  s.get_fact("user","editor").unwrap().unwrap().id); // unchanged = no-op
       assert_eq!(s.list_facts("user", Some("edi")).unwrap().len(), 1);
       assert_eq!(s.forget_fact("user","editor").unwrap(), 2);   // active + history
       assert!(s.get_fact("user","editor").unwrap().is_none());
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres pg_facts` → `Unsupported`/assertion.
3. **Minimal impl:** replace the `FactsStore` stub. `set_fact` in a
   **transaction** (supersede active row via `UPDATE … SET superseded_at=$now
   WHERE entity=$1 AND key=$2 AND superseded_at IS NULL AND value<>$3`, then
   `INSERT`; if value unchanged, return existing id). `get_fact`/`list_facts`
   filter `superseded_at IS NULL`; `history` returns all rows `ORDER BY
   created_at DESC`; `forget_fact` hard-deletes; `facts_stats` aggregates.
   All via `try_get` + `db_err`.
4. **Confirm pass / commit:** green → `feat(store): FactsStore on Postgres (versioned facts + history)`.

### T4 — FeedbackStore on Postgres (6 methods, tsvector search)

1. **Failing test:** store → tsvector search hit + applied increment + stats.
   ```rust
   #[test]
   fn pg_feedback_search_and_apply() {
       let Some(s) = pg_test_store() else { return };
       let id = s.store_feedback(Feedback::new("routing","ctx","predicted X","use Y instead", Some("reason"))).unwrap();
       let hits = s.search_feedback("instead", None, 10).unwrap();
       assert!(hits.iter().any(|f| f.id == id));
       assert_eq!(s.list_feedback(Some("routing"), 10).unwrap().len(), 1);
       s.increment_applied(&id).unwrap();
       assert_eq!(s.feedback_stats().unwrap().total, 1);
       s.delete_feedback(&id).unwrap();
       assert!(s.search_feedback("instead", None, 10).unwrap().is_empty());
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres pg_feedback`.
3. **Minimal impl:** replace the `FeedbackStore` stub. `search_feedback` =
   `WHERE tsv @@ plainto_tsquery('simple',$1) [AND topic=$2] ORDER BY
   ts_rank(tsv, plainto_tsquery('simple',$1)) DESC LIMIT $3`; others map 1:1
   to SQLite semantics (`applied_count`, `created_at`).
4. **Confirm pass / commit:** green → `feat(store): FeedbackStore on Postgres (tsvector search)`.

### T5 — TranscriptStore on Postgres (9 methods, tsvector search + replay)

1. **Failing test:** session/message store + chronological replay + search + cascade delete.
   ```rust
   #[test]
   fn pg_transcript_store_search_replay() {
       let Some(s) = pg_test_store() else { return };
       let sid = s.ensure_session("sess-1","claude",Some("icm"),None).unwrap();
       assert_eq!(s.ensure_session("sess-1","claude",Some("icm"),None).unwrap(), sid); // idempotent
       s.record_message(&sid, Role::User, "how does routing work", None, None, None).unwrap();
       s.record_message(&sid, Role::Assistant, "routing uses the store enum", None, Some(12), None).unwrap();
       assert_eq!(s.list_session_messages(&sid, 100, 0).unwrap().len(), 2);      // chronological
       let hits = s.search_transcripts("routing", None, Some("icm"), 10).unwrap();
       assert!(!hits.is_empty());
       s.forget_session(&sid).unwrap();
       assert!(s.get_session(&sid).unwrap().is_none());
       assert_eq!(s.list_session_messages(&sid, 100, 0).unwrap().len(), 0);      // cascaded
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres pg_transcript`.
3. **Minimal impl:** replace the `TranscriptStore` stub. `create_session`
   (random id) vs `ensure_session` (`INSERT … ON CONFLICT (id) DO NOTHING`
   then select); `record_message` inserts + bumps `sessions.updated_at`;
   `search_transcripts` = `messages.tsv @@ plainto_tsquery('simple',$1)` joined
   to `sessions` for the optional `project` filter, `ORDER BY ts_rank … DESC`,
   returning `TranscriptHit`; `forget_session` deletes (FK cascade drops
   messages); `transcript_stats` aggregates.
4. **Confirm pass / commit:** green → `feat(store): TranscriptStore on Postgres (store/search/replay)`.

### T6 — MemoirStore part 1: memoir + concept CRUD (11 methods)

1. **Failing test:** memoir + concept CRUD round-trip incl. `UNIQUE(memoir_id,name)`.
   ```rust
   #[test]
   fn pg_memoir_and_concept_crud() {
       let Some(s) = pg_test_store() else { return };
       let mid = s.create_memoir(Memoir::new("arch","desc")).unwrap();
       assert_eq!(s.get_memoir_by_name("arch").unwrap().unwrap().id, mid);
       let cid = s.add_concept(Concept::new(&mid,"store enum","runtime backend dispatch")).unwrap();
       assert_eq!(s.get_concept_by_name(&mid,"store enum").unwrap().unwrap().id, cid);
       assert_eq!(s.list_concepts(&mid).unwrap().len(), 1);
       let mut c = s.get_concept(&cid).unwrap().unwrap(); c.confidence = 0.9;
       s.update_concept(&c).unwrap();
       assert_eq!(s.get_concept(&cid).unwrap().unwrap().confidence, 0.9);
       s.delete_concept(&cid).unwrap();
       assert!(s.get_concept(&cid).unwrap().is_none());
       s.delete_memoir(&mid).unwrap();
       assert_eq!(s.list_memoirs().unwrap().len(), 0);
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres pg_memoir_and_concept_crud`.
3. **Minimal impl:** implement the 6 memoir + 5 concept CRUD methods (leave the
   remaining `MemoirStore` methods as stubs *temporarily* — noted in the
   Manifest as staged-within-feature, all removed by T8). JSON `labels`/
   `source_memory_ids` serialized to TEXT via serde, mirroring SQLite.
4. **Confirm pass / commit:** green → `feat(store): MemoirStore CRUD on Postgres (memoir + concept)`.

### T7 — MemoirStore part 2: search + refine (5 methods)

1. **Failing test:** FTS + label search + cross-memoir search + refine bumps revision.
   ```rust
   #[test]
   fn pg_memoir_search_and_refine() {
       let Some(s) = pg_test_store() else { return };
       let mid = s.create_memoir(Memoir::new("arch","d")).unwrap();
       let cid = s.add_concept(Concept::new(&mid,"routing","dispatch via store enum")).unwrap();
       assert!(s.search_concepts_fts(&mid,"dispatch",10).unwrap().iter().any(|c| c.id==cid));
       assert!(s.search_all_concepts_fts("dispatch",10).unwrap().iter().any(|c| c.id==cid));
       s.refine_concept(&cid,"dispatch via runtime store enum",&["m1".into()]).unwrap();
       assert_eq!(s.get_concept(&cid).unwrap().unwrap().revision, 2);
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres pg_memoir_search_and_refine`.
3. **Minimal impl:** `search_concepts_fts`/`search_all_concepts_fts` via
   `concepts.tsv @@ plainto_tsquery('simple',$q)` (scoped by `memoir_id` for
   the former) `ORDER BY ts_rank`; `search_concepts_by_label` matches the JSON
   `labels` TEXT (mirror SQLite's label match); `refine_concept` updates
   definition, merges `source_memory_ids`, bumps `revision`, sets `updated_at`.
4. **Confirm pass / commit:** green → `feat(store): MemoirStore search + refine on Postgres (tsvector)`.

### T8 — MemoirStore part 3: graph + stats (9 methods; remove last stub)

0. **Read first:** the SQLite `get_neighbors`/`get_neighborhood` impl — capture
   exact link direction + dedup semantics to mirror.
1. **Failing test:** links, directional neighbors, depth-bounded neighborhood, stats.
   ```rust
   #[test]
   fn pg_memoir_graph_and_stats() {
       let Some(s) = pg_test_store() else { return };
       let m = s.create_memoir(Memoir::new("arch","d")).unwrap();
       let a = s.add_concept(Concept::new(&m,"A","a")).unwrap();
       let b = s.add_concept(Concept::new(&m,"B","b")).unwrap();
       let c = s.add_concept(Concept::new(&m,"C","c")).unwrap();
       s.add_link(ConceptLink::new(&a,&b,Relation::RelatedTo)).unwrap();
       s.add_link(ConceptLink::new(&b,&c,Relation::RelatedTo)).unwrap();
       assert_eq!(s.get_links_from(&a).unwrap().len(), 1);
       assert_eq!(s.get_links_to(&c).unwrap().len(), 1);
       assert!(s.get_neighbors(&a, None).unwrap().iter().any(|n| n.id==b));
       let (nodes, links) = s.get_neighborhood(&a, 2).unwrap(); // A→B→C
       assert!(nodes.iter().any(|n| n.id==c) && links.len()>=2);
       assert_eq!(s.get_links_for_memoir(&m).unwrap().len(), 2);
       assert_eq!(s.memoir_stats(&m).unwrap().concept_count, 3);
       assert_eq!(*s.batch_memoir_concept_counts().unwrap().get(&m).unwrap(), 3);
   }
   ```
2. **Confirm fail:** `cargo test -p icm-store --features postgres pg_memoir_graph_and_stats`.
3. **Minimal impl:** `add_link`/`get_links_from`/`get_links_to`/`delete_link`
   1:1; `get_neighbors` joins links→concepts with optional `relation` filter;
   `get_neighborhood(depth)` via `WITH RECURSIVE walk AS (base ⋃ step)` with a
   level counter `< depth` and `UNION` dedup, returning the reached concepts +
   the traversed links; `get_links_for_memoir` batch-joins; `memoir_stats` +
   `batch_memoir_concept_counts` aggregate with `GROUP BY memoir_id`. **All
   `MemoirStore` stubs are now gone** (Manifest staged-cut closed).
4. **Confirm pass / commit:** green → `feat(store): MemoirStore graph traversal + stats on Postgres`.

### T9 — Docs + optional CI job + header note

1. **Verification (commands):**
   `grep -q "tsvector\|full subsystem\|facts.*memoir.*feedback\|transcript" docs/postgres-backend.md` &&
   `grep -qi "tenant" docs/postgres-backend.md` &&
   `grep -q "pgvector/pgvector" .github/workflows/ci.yml`.
2. **Implement:** `docs/postgres-backend.md` — replace the "core memory only"
   framing with full-subsystem support; add the local `docker run
   pgvector/pgvector:pg16` + `ICM_POSTGRES_URL` test workflow; note the
   pre-seeded nullable `tenant` columns and that isolation is F-003b. Fix the
   `postgres.rs` header note (subsystems no longer `Unsupported`). Add a CI job
   in `ci.yml` with a `pgvector/pgvector` service container exporting
   `ICM_POSTGRES_URL` and running `cargo test -p icm-store --features postgres`
   (may be `continue-on-error`/opt-in per "CI optional").
3. **Commit:** `docs(store): PG full-subsystem support + docker test workflow + optional CI job`.

### T10 — Full gate + zero-regression

1. **Verification:**
   - `cargo fmt --all -- --check`
   - `cargo clippy -p icm-store --features postgres --all-targets -- -D warnings`
   - `cargo clippy --workspace --all-targets -- -D warnings` (default)
   - `cargo test --workspace` (default, **no** `ICM_POSTGRES_URL`) → green, PG tests skip → **zero-regression on SQLite**.
   - `ICM_POSTGRES_URL=… cargo test -p icm-store --features postgres` → all `pg_*` green against live PG.
   - `cargo build --no-default-features --features postgres` compiles (no `Unsupported` for the 4 subsystems: `! grep -n 'Unsupported' postgres.rs` in the four `impl` blocks).
2. **Implement:** fix any gate issue.
3. **Commit:** `chore(f-003a): verify gates + zero-regression`.

## Test Plan

- **Unit (gated `pg_tests`, per method group):** T1 schema+idempotency+tenant;
  T3 facts versioning/history/no-op/forget; T4 feedback search/list/apply/
  stats/delete; T5 transcript ensure-idempotent/record/replay-order/search/
  cascade/stats; T6 memoir+concept CRUD + uniqueness; T7 concept FTS + label +
  cross-memoir + refine-revision; T8 links/neighbors(directional)/neighborhood
  (depth+cycle)/links-for-memoir/stats/batch-counts.
- **Integration:** through the `Store` enum with `ICM_DB_BACKEND=postgres` —
  each trait reachable via the enum (dispatch already wired) round-trips.
- **E2E:** `ICM_DB_BACKEND=postgres` + live PG: `icm facts set/get/history`,
  `icm memoir` concept+link+neighborhood, `icm feedback` search, `icm sessions
  search` exercise the four subsystems end-to-end against Postgres.
- **Regression:** `cargo test --workspace` with **no** `ICM_POSTGRES_URL` →
  every `pg_*` test skips, SQLite suite unchanged; `cargo build` (default,
  no `postgres` feature) unaffected; existing PG `MemoryStore` tests still pass.

## Security Review

- **Static:** `cargo clippy --features postgres -- -D warnings`; `cargo audit`
  (no new deps — `postgres`/`pgvector` already vetted; nothing added).
- **Threat model (new surface):**
  - **SQL injection:** every query uses **parameterized** statements
    (`$1,$2,…` / `client.query(sql, &[&param])`); no string interpolation of
    user values — including FTS (`plainto_tsquery($1)`) and `LIKE`/prefix
    filters (bind the pattern). Named check: grep the new impls for `format!`
    used to build SQL with data — must be none (only static SQL + binds).
  - **Panic-as-DoS:** no `unwrap`/`expect`/`Row::get`; `try_get` + `db_err`
    everywhere so malformed rows return typed errors, not aborts.
  - **Tenant columns are inert:** pre-seeded `tenant` is never read/written by
    F-003a — it cannot leak or mis-scope data because it is unused (isolation
    is F-003b). No cross-tenant surface is introduced or implied.
  - **Resource use:** `get_neighborhood` recursion is depth-bounded; searches
    are `LIMIT`-bounded — no unbounded scans from user input.
- **Specific checks:** input validation (bind all params); secrets (none —
  connection string via env, already handled by existing PG connect, never
  logged); injection (parameterized); no SSRF/path traversal (DB-only).

## Logic Review Checkpoints

- **CP-A (after T1/T2):** schema mirrors SQLite columns/constraints exactly
  (FKs, CASCADE, `UNIQUE`, `CHECK`, facts partial-unique); tsvector uses
  `simple`; every new table has a nullable `tenant`; `init_schema` idempotent;
  harness skips cleanly without `ICM_POSTGRES_URL`.
- **CP-B (after T3/T4/T5):** `set_fact` is transactional; timestamps TEXT
  RFC3339 sort chronologically; FTS via bound `plainto_tsquery`; cascade
  deletes verified; no `Row::get`, no `format!`-built SQL with data.
- **CP-C (after T6/T7/T8):** graph direction + dedup match SQLite;
  `get_neighborhood` depth bound + cycle-safe; **no `Unsupported` remains** in
  the four `impl` blocks; N+1 avoided in `get_links_for_memoir` /
  `batch_memoir_concept_counts`.
- **CP-D (docs, T9):** doc no longer claims "core memory only"; tenant/F-003b
  boundary stated; header note corrected.

## G4 contract delta

Appended to the project contract's Definition of Done after this plan:

1. `cargo build --no-default-features --features postgres` compiles with all
   four subsystem traits implemented (no `Unsupported` for
   facts/memoir/feedback/transcript).
2. `clippy -D warnings` on `--features postgres` and on the default workspace.
3. Live-PG gated tests pass:
   `ICM_POSTGRES_URL=… cargo test -p icm-store --features postgres` (all `pg_*`).
4. Default `cargo test --workspace` (no `ICM_POSTGRES_URL`) green — PG tests
   skip; SQLite behavior byte-identical (zero-regression).
5. Every new PG table carries a nullable `tenant` column; `memories`
   unchanged (F-003b owns it).
6. `init_schema` idempotent (running twice is a no-op).
7. `docs/postgres-backend.md` updated (full-subsystem support + docker test
   workflow + tenant/F-003b boundary).

## Logic Completeness Manifest

**Every requirement in the linked spec MUST be implemented in full.
Authorized simplifications: (none).**

Staged-within-feature note (NOT a cut): `MemoirStore` is implemented across
T6→T7→T8; methods not yet reached remain `Unsupported` **only between those
tasks** and are ALL implemented by the end of T8. CP-C verifies no
`Unsupported` remains in the four `impl` blocks. No method ships stubbed.

Verification-environment note: the `pg_*` tests require a live Postgres. If
execution cannot provision one (docker probed available now), it pauses and
asks the user for `ICM_POSTGRES_URL` rather than marking PG behavior verified
without running it.

### Spec Coverage Matrix

| SC-ID | Capability | Implementing task(s) | Verification command |
|---|---|---|---|
| SC-1 | FactsStore on PG (versioning + history) | T3 | `ICM_POSTGRES_URL=… cargo test -p icm-store --features postgres pg_facts` |
| SC-2 | MemoirStore on PG (CRUD + search + graph) | T6, T7, T8 | `… cargo test -p icm-store --features postgres pg_memoir` |
| SC-3 | FeedbackStore on PG (tsvector search) | T4 | `… cargo test -p icm-store --features postgres pg_feedback` |
| SC-4 | TranscriptStore on PG (store/search/replay) | T5 | `… cargo test -p icm-store --features postgres pg_transcript` |
| SC-5 | Idempotent PG schema (7 tables) | T1 | `… cargo test -p icm-store --features postgres pg_schema_has_all` |
| SC-6 | tsvector + GIN full-text search | T1 (schema), T4/T5/T7 (use) | `… pg_feedback_search_and_apply` / `pg_transcript_store_search_replay` / `pg_memoir_search_and_refine` |
| SC-7 | Nullable `tenant` pre-seeded on new tables | T1 | `… pg_schema_has_all_subsystem_tables_and_tenant` |
| SC-8 | Env-gated test harness (skip w/o URL) | T2 | `cargo test -p icm-store --features postgres` (no URL → skips) + `pg_harness_connects_and_resets` |
| SC-9 | Optional CI postgres+pgvector job | T9 | `grep -q "pgvector/pgvector" .github/workflows/ci.yml` |
| SC-10 | Zero-regression + hygiene + docs | T9, T10 | `cargo test --workspace` (no URL) + `cargo clippy … -D warnings` + `grep -qi tenant docs/postgres-backend.md` |

All SC-1..SC-10 mapped to ≥1 task. Manual coverage check (no
`scripts/check-spec-coverage.sh` in repo) — declared degraded/manual, no
orphans.

## File Size Constraints

| File | Projected | Threshold | Flag |
|---|---|---|---|
| `crates/icm-store/src/postgres.rs` | ~2800 lines (from ~1730; +~46 methods, schema, tests) | Persistence backend (relaxed) | OK (relaxed) — mirrors `store.rs` ~4000-line SQLite backend; same repo convention (one file per backend). If it exceeds ~3500, split the gated `pg_tests` module into `postgres_tests.rs` (test files are relaxed). |
| `docs/postgres-backend.md` | ~180 lines | Docs (relaxed) | OK (relaxed) |
| `.github/workflows/ci.yml` | +~25 lines | Config (relaxed) | OK (relaxed) |
| `.zeus/features.md` | +1 line edit | Docs (relaxed) | OK (relaxed) |

`postgres.rs` is the only substantive-code file; it stays under the
established SQLite backend precedent. No `OVER` rows.

**User-approved:** 2026-07-23 by rainhan@coupert.com
