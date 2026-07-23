# PostgreSQL backend (opt-in)

By default ICM stores memory in a node-local SQLite file. That is perfect
for a single machine, but a local file cannot be shared between several
ICM processes or between Kubernetes replicas, so memory can't be shared
across instances.

The **PostgreSQL backend** (issue #301) runs the same memory model over a
network-accessible PostgreSQL database. Every instance reads and writes
one shared store, and PostgreSQL serialises concurrent writers, so N
replicas can `icm store` into the same memory with no lost rows.

It is **opt-in** and selected at build time via a Cargo feature. The
default build is unchanged: SQLite only, single binary, zero external
services.

## What it covers

As of **F-003a**, the PostgreSQL backend is a **complete replacement for
SQLite across all subsystems** — a central Postgres node serves the full
feature set, not just core memory.

**Core memory:**

- `store` (with dedup + metadata merge), `get`, `update`, `forget`
- keyword search, full-text search (PostgreSQL `tsvector` + GIN),
  vector KNN (`pgvector`, cosine), and the hybrid blend (30% FTS / 70%
  vector)
- `list`, `topics`, `stats`, `health`
- temporal `decay` (access-aware) and `prune`
- the ancillary tables used by the normal store/recall/hook path:
  hook telemetry, the async extraction queue, code areas, and the
  key/value metadata.

**All other subsystems (F-003a):**

- **facts** — versioned `(entity, key, value)` with supersession history
- **memoir** — memoirs, concepts, concept links, graph traversal
  (`get_neighbors`, depth-bounded `get_neighborhood`), cycle rejection
- **feedback** — store + `tsvector` full-text search
- **transcripts** — sessions + messages, verbatim replay, `tsvector` search

Full-text search on these uses the same PostgreSQL `tsvector('simple')`
GENERATED columns + GIN indexes as core memory. `ts_rank`/recency ordering
differs slightly from SQLite's FTS5 `bm25`, but hit membership matches.

## Multi-tenant isolation (RLS, F-003b)

A central Postgres node can serve **isolated tenants**: each tenant reads and
writes only its own rows, enforced by PostgreSQL **Row-Level Security** — not
by application code, so a missed filter cannot leak across tenants.

How it fits together:

1. **Identity (F-003):** the HTTP server maps each `Authorization: Bearer
   <token>` to a tenant via the `[remote] tokens` map (see
   `docs/remote-backend.md` §6).
2. **Scope (F-003b):** each store-serving request sets the tenant on its
   connection — `SELECT set_config('app.tenant', $1, false)` (the tenant is a
   **bound parameter**, never interpolated) — on the same locked connection
   as the query.
3. **Enforcement (F-003b):** every tenant-scoped table has
   `ENABLE`/`FORCE ROW LEVEL SECURITY` and a policy that allows a row iff
   `tenant = current_setting('app.tenant', true)`. Writes auto-tag via a
   column `DEFAULT current_setting('app.tenant', true)`.

When `app.tenant` is unset (`NULL`/`''`) the policy is **unrestricted** — the
single-dataset behavior used by direct CLI/admin access and by
single-token / no-tokens-map deployments. Legacy rows created before F-003b
are migrated to the `'default'` tenant on first connect.

> ### ⚠️ Deployment requirements & limits
>
> - **Connect as a NON-SUPERUSER role.** PostgreSQL **bypasses RLS for
>   superusers** (and for `BYPASSRLS` roles) — even with `FORCE`. The
>   `ICM_POSTGRES_URL` role for a multi-tenant node MUST be an ordinary
>   (non-superuser) role granted access to the ICM tables, or isolation is
>   silently NOT enforced. (`FORCE ROW LEVEL SECURITY` covers the case where
>   that role also owns the tables.)
> - **SQLite is not isolated.** SQLite has no RLS; the SQLite backend always
>   serves one dataset. Isolation is a Postgres-only guarantee.
> - **Direct CLI/admin access is unrestricted.** `icm` run directly against
>   Postgres (not via the HTTP node) leaves `app.tenant` unset → sees all
>   tenants. The isolation boundary is the HTTP-served path (thin clients).
> - **Session `SET`, single connection.** The tenant is set at session scope
>   on the store's single guarded connection (safe because access is
>   serialized). Introducing a **connection pool** later would require
>   switching to `SET LOCAL` inside a per-request transaction.

## HTTP / web servers over Postgres (F-003c)

`icm serve --http` and `icm serve --web` support the Postgres backend. The
blocking Postgres client cannot run on a tokio-managed thread (its internal
`block_on` would panic "Cannot start a runtime from within a runtime" — this
holds for `spawn_blocking` pool threads too, not just async workers), so both
servers run every store operation on a **dedicated store-actor thread**
(a plain `std::thread` that owns the `Store`, off the runtime). This is
transparent to callers and to the SQLite backend; it also serializes store
access (as the previous `Mutex` did) and contains any store panic as a `500`
rather than a downed server.

## Requirements

PostgreSQL with the [`pgvector`](https://github.com/pgvector/pgvector)
extension available (the backend runs `CREATE EXTENSION IF NOT EXISTS
vector`). The `pgvector/pgvector` images and Azure Database for
PostgreSQL Flexible Server (with `vector` allow-listed) both work.

## Build

The **default `icm` binary already includes this backend** (backends are
additive and selected at runtime, like SurrealDB's `Surreal<Any>`). Nothing
to build — just configure it. For a lean SQLite-only binary, build with
`--no-default-features --features "embeddings,tui,http-api,backend-sqlite"`.

## Configure

Select the backend at runtime and give it a connection string:

```sh
export ICM_DB_BACKEND=postgres
export ICM_POSTGRES_URL="postgres://user:pass@host:5432/icm"
# DATABASE_URL is accepted as a fallback for the URL.
```

With `ICM_DB_BACKEND` unset (or `sqlite`) the binary uses the local SQLite
file as before. The `--db` flag (a SQLite file path) is ignored by this
backend.

The schema — including the `vector(N)` embedding column whose dimension
`N` matches your embedder — is created on first connect. The stored
dimension is authoritative afterwards, so always initialise a database
with the embedder you intend to use (or `--no-embeddings` for
keyword/FTS-only).

## Verify

```sh
docker run -d --name icm-pg \
    -e POSTGRES_PASSWORD=icm -e POSTGRES_USER=icm -e POSTGRES_DB=icm \
    -p 55432:5432 pgvector/pgvector:pg16

export ICM_DB_BACKEND=postgres
export ICM_POSTGRES_URL="postgres://icm:icm@127.0.0.1:55432/icm"

icm store -t demo -c "PostgreSQL backend shares memory across replicas" -i high
icm recall "shared memory"
icm stats

# Integration tests — one round-trip per subsystem (facts, memoir, feedback,
# transcript) plus schema/harness. They read ICM_POSTGRES_URL and SKIP
# automatically when it is unset, so the default `cargo test` is unaffected.
# Run single-threaded: the tests share one database and truncate between runs.
ICM_POSTGRES_URL="postgres://icm:icm@127.0.0.1:55432/icm" \
  cargo test -p icm-store --features postgres pg_ -- --test-threads=1
```

## Kubernetes

A network backend is what makes a horizontally-scaled deployment share
memory. Point every replica's `ICM_POSTGRES_URL` at the same PostgreSQL
service and they read/write one store. See
[`deploy/k8s`](../deploy/k8s) for a minimal manifest set used to validate
this on a real cluster.
