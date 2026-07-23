# ICM Deployment Guide

How to run ICM beyond a single laptop: from one local file, to a team sharing
one project's memory, to a **multi-tenant central node** on Postgres with
per-tenant data isolation.

- Single machine → [Local (default)](#local-default)
- A team, one shared dataset → [Team: shared central node](#team-shared-central-node)
- Many teams, isolated → [Multi-tenant (Postgres RLS)](#multi-tenant-postgres-rls)

Backends are chosen **at runtime** via `ICM_DB_BACKEND`
(`sqlite` (default) | `postgres` | `opensearch` | `remote`); the published
binary embeds all of them, so deployment is configuration, not a rebuild.

---

## Local (default)

Nothing to deploy — one SQLite file, no services. See the main
[README](../README.md). The rest of this guide is for shared/central setups.

---

## Team: shared central node

One central node owns the database and the embedding model; every dev machine
is a **thin client** that carries no model and no local DB (it sends text, the
node embeds and stores). This is the three-tier topology (issue F-001):

```
dev A (icm thin client) ┐
dev B (icm thin client) ┼── HTTP /rpc ──► central: icm serve --http ──► Postgres / SQLite
dev C (icm thin client) ┘                    warm embedder (local or cloud)
```

### 1. Central node — Postgres backend

Use Postgres (with `pgvector`) so multiple instances/replicas share one store.
Bring up Postgres (managed, or the demo container):

```bash
docker run -d --name icm-pg \
  -e POSTGRES_USER=icm -e POSTGRES_PASSWORD=icm -e POSTGRES_DB=icm \
  -p 5432:5432 pgvector/pgvector:pg16
```

Run the node against it (single shared token guards the API):

```bash
export ICM_DB_BACKEND=postgres
export ICM_POSTGRES_URL="postgres://icm:icm@localhost:5432/icm"
icm serve --http 0.0.0.0:11435 --token "$ICM_SHARED_TOKEN"
```

The schema (tables, `vector(N)` column, FTS `tsvector` + GIN indexes) is
created idempotently on first connect. Full backend reference:
[docs/postgres-backend.md](postgres-backend.md).

### 2. Thin clients

Point each machine at the central node — works for the CLI, the Claude Code
MCP stdio server (`icm serve`), and the PostToolUse hook alike:

```bash
export ICM_DB_BACKEND=remote
export ICM_REMOTE_URL=http://central-host:11435
export ICM_REMOTE_TOKEN="$ICM_SHARED_TOKEN"     # required if the node set --token

icm recall "database schema"     # embeds server-side, returns hits
icm store -t decisions -c "…"    # stored in the central DB, shared with everyone
```

A lean client needs neither embeddings nor a local backend:

```bash
cargo build --release -p icm-cli --no-default-features --features "backend-sqlite,remote-store"
```

Full reference: [docs/remote-backend.md](remote-backend.md).

---

## Multi-tenant (Postgres RLS)

For many teams/users on **one** Postgres node with each tenant isolated to its
own data. This combines: token→tenant identity (F-003), full-subsystem
Postgres (F-003a), Row-Level Security isolation (F-003b), and the async
HTTP/web fix that makes it actually serve over Postgres (F-003c).

### ⚠️ The one requirement that is easy to miss

**The central node MUST connect to Postgres as a NON-SUPERUSER role.**
PostgreSQL bypasses RLS for superusers (and `BYPASSRLS` roles) — *even with
`FORCE ROW LEVEL SECURITY`*. The default `POSTGRES_USER` in the postgres/
pgvector images is a **superuser**, so connecting as it silently disables
isolation. Connect as an ordinary role that owns the ICM tables.

### 1. Create a non-superuser app role

As an admin/superuser, once per database:

```sql
CREATE ROLE icm_app LOGIN PASSWORD 'change-me' NOSUPERUSER;
GRANT ALL ON SCHEMA public TO icm_app;
CREATE EXTENSION IF NOT EXISTS vector;   -- superuser installs the extension
```

The ICM tables must be **owned by `icm_app`** so that `FORCE ROW LEVEL
SECURITY` applies to it. The simplest way: let `icm_app` create them — start
the node as `icm_app` against an empty database and `init_schema` creates +
owns everything (and enables/forces RLS). If tables already exist under
another owner, reassign them (`ALTER TABLE … OWNER TO icm_app`) before first
run.

### 2. Configure the token→tenant map

On the central node, in `~/.config/icm/config.toml` (or `$ICM_CONFIG`):

```toml
[remote.tokens]
"s3cr3t-token-team-a" = "team-a"
"s3cr3t-token-team-b" = "team-b"
```

Each token maps to a tenant name. Unknown/missing tokens are rejected with
`401`. Tokens are secrets — they are never logged, echoed, or put in error
messages; only the resolved tenant is observable. Restrict `config.toml`
permissions (operator's responsibility).

### 3. Run the node as the non-superuser role

```bash
export ICM_DB_BACKEND=postgres
export ICM_POSTGRES_URL="postgres://icm_app:change-me@pg-host:5432/icm"
export ICM_CONFIG=/etc/icm/config.toml     # the [remote.tokens] file above
icm serve --http 0.0.0.0:11435
```

Each request's Bearer token → tenant → `SET app.tenant` on the connection →
RLS filters every read and write to that tenant, and inserts auto-tag with it.
The `icm serve --web` admin dashboard also works over Postgres (it runs
unscoped = sees all tenants — an admin view).

### 4. Verify isolation

```bash
N=http://central:11435
curl -s "$N/whoami" -H "Authorization: Bearer s3cr3t-token-team-a"   # {"tenant":"team-a"}

# team-a writes; team-b writes; each /stats sees only its own count:
curl -s -X POST "$N/store" -H "Authorization: Bearer s3cr3t-token-team-a" \
  -H 'content-type: application/json' -d '{"topic":"t","content":"A only"}'
curl -s -X POST "$N/store" -H "Authorization: Bearer s3cr3t-token-team-b" \
  -H 'content-type: application/json' -d '{"topic":"t","content":"B only"}'
curl -s "$N/stats?format=json" -H "Authorization: Bearer s3cr3t-token-team-a"  # total_memories: 1
curl -s "$N/stats?format=json" -H "Authorization: Bearer s3cr3t-token-team-b"  # total_memories: 1

# No token → 401. Unknown token → 401.
curl -s -o /dev/null -w '%{http_code}\n' "$N/stats"                             # 401
```

Backend/RLS reference: [docs/postgres-backend.md](postgres-backend.md)
("Multi-tenant isolation (RLS)").

---

## Cloud embeddings (optional)

The central node can embed via any OpenAI-compatible endpoint instead of the
local model — in `config.toml`:

```toml
[embeddings]
provider = "openai"
model = "text-embedding-3-small"
base_url = "https://api.openai.com/v1"
dimensions = 1536
```

```bash
export ICM_EMBED_API_KEY=sk-...      # falls back to OPENAI_API_KEY; never stored/logged
```

Repeated embeddings are cached (disk + in-memory LRU); watch hit-rate at
`GET /cache` (HTML) or `GET /cache/stats` (JSON). Requires the
`cloud-embeddings` build feature (in the published binary).

---

## Docker / Kubernetes

- **Image:** [`deploy/Dockerfile.postgres`](../deploy/Dockerfile.postgres)
  builds a slim `icm` with `--features "postgres,http-api"` (no embedding
  model). Add `web`/`remote-store`/`cloud-embeddings` features as needed.
- **Manifests:** [`deploy/k8s/postgres.yaml`](../deploy/k8s/postgres.yaml)
  (Secret + pgvector Deployment + Service) and an
  [`icm-writer-job`](../deploy/k8s/icm-writer-job.yaml). These are **single-
  tenant / dev** examples — the connection string uses the superuser `icm`.
  For multi-tenant, switch `ICM_POSTGRES_URL` to the **non-superuser** role
  (above) and mount the `[remote.tokens]` config; scale the node behind a
  Service and point thin clients at it.

---

## Security checklist

- [ ] **Non-superuser role** for the central node's `ICM_POSTGRES_URL`
      (superusers bypass RLS — isolation silently off otherwise).
- [ ] **Tokens are secrets:** restrict `config.toml`; they never hit logs.
- [ ] **TLS:** the HTTP API is plain HTTP — put it behind a TLS-terminating
      proxy or a tunnel (WireGuard / Tailscale / SSH `-L`) for anything off a
      trusted network. The Bearer token guards access, not eavesdropping.
- [ ] `--token` (or a `[remote] tokens` map) is set on any non-localhost bind.
- [ ] Postgres reachable only from the central node(s), not the public net.

### Honest boundaries (what is NOT isolated)

- **SQLite is never isolated** — it has no RLS. Isolation is Postgres-only.
- **Direct CLI/admin access to Postgres is unrestricted** — a client that
  connects straight to the DB (not via the HTTP node) runs with no tenant set
  = sees everything. The isolation boundary is the HTTP-served path (thin
  clients). Keep direct DB creds to trusted admins.
- **One shared connection** on the node (session-scoped `SET app.tenant`);
  adding a connection pool later requires switching to `SET LOCAL` in a
  transaction (see postgres-backend.md).

---

## Operations

- **Idempotent schema/migrations:** every connect re-runs `init_schema`
  (create-if-not-exists, add-column-if-not-exists, backfill NULL→`default`
  tenant, enable/force RLS). Re-running or rolling a new binary is safe.
- **Backup:** standard `pg_dump` / managed-Postgres snapshots. (SQLite: copy
  the file.)
- **Performance:** `icm bench --count 5000` — baselines in
  [docs/bench-baseline.md](bench-baseline.md). The default (SQLite) hot path
  is unchanged by any of the shared-backend work.
- **Upgrades:** roll the central node binary; the schema migration is
  idempotent. Thin clients need no coordinated upgrade (they only speak HTTP).

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| client: `Connection refused` | node not running / wrong `ICM_REMOTE_URL` / firewall |
| client/API: `401` | missing/incorrect token vs the node's `--token` or `[remote] tokens` |
| every tenant sees every row | node connected as a **superuser** — use a non-superuser role (RLS is bypassed for superusers) |
| `ICM_DB_BACKEND=remote requires ICM_REMOTE_URL` | set `ICM_REMOTE_URL` on the client |
| `embedding dimension N does not match store dimension M` | the model/dims changed; re-embed on the node (`icm embed --force`) |
| memoir/facts/transcript ops fail on an old node | pre-F-003a nodes only supported core memory on Postgres — upgrade the node binary |
| server panic `Cannot start a runtime from within a runtime` | pre-F-003c binary serving over Postgres — upgrade the node (store ops now run on a dedicated thread) |
