# Remote backend — three-tier shared memory (F-001)

Run one central `icm` node that owns the database and embedding, and let
any number of dev machines share it as **thin clients** — no model
download, no local database. Optionally the central node computes
embeddings via a cloud (OpenAI-compatible) endpoint instead of a local
model.

```
dev machine A (icm thin client) ┐
dev machine B (icm thin client) ┼── HTTP /rpc ──► central: icm serve --http ──► SQLite
dev machine C (icm thin client) ┘                  │  warm embedder (local or cloud)
                                                    │  embedding cache (disk + LRU)
                                                    └─ /cache dashboard
```

**Invariant:** in remote mode the client loads **no embedder** — it sends
text, and the server embeds. This is what makes clients zero-model /
zero-DB. Never enable a local model on a client expecting it to embed;
`ICM_DB_BACKEND=remote` forces embeddings off on the client.

## 1. Central node

Use the **SQLite** backend — it is the only one that supports the full
surface (memory + facts + memoir + feedback + transcripts):

```bash
# choose a stable bind address reachable on your LAN, and a shared token
ICM_DB_BACKEND=sqlite \
  icm serve --http 0.0.0.0:11435 --token "$ICM_SHARED_TOKEN"
```

To embed on the server via a cloud endpoint (optional), set in
`~/.config/icm/config.toml`:

```toml
[embeddings]
provider = "openai"
model = "text-embedding-3-small"
base_url = "https://api.openai.com/v1"
dimensions = 1536
```

and export the key (never stored on disk, never logged):

```bash
export ICM_EMBED_API_KEY=sk-...      # falls back to OPENAI_API_KEY
```

Build note: the cloud embedder requires `--features cloud-embeddings`
(the published binary includes it; a lean build enables it explicitly).

## 2. Client machines

Point each client at the central node. This works transparently for the
`icm` CLI, the Claude Code MCP stdio server (`icm serve`), and the
PostToolUse hook — all go through the same backend selector:

```bash
export ICM_DB_BACKEND=remote
export ICM_REMOTE_URL=http://<central-host>:11435
export ICM_REMOTE_TOKEN="$ICM_SHARED_TOKEN"   # optional; required if server set --token

icm recall "database schema"     # embeds server-side, returns hits
icm store -t decisions -c "..."  # stored in the central DB, shared with all machines
```

A lean client binary needs neither embeddings nor a local backend beyond
the selector:

```bash
cargo build --release -p icm-cli \
  --no-default-features --features "backend-sqlite,remote-store"
```

## 3. Security (phase 1 — LAN)

- Transport is **plain HTTP**; the Bearer token guards against accidental
  cross-connection, not eavesdropping. Deploy on a trusted LAN.
- For traffic that leaves a trusted network, wrap it in a tunnel
  (SSH `-L`, WireGuard, Tailscale) or a TLS-terminating reverse proxy.
  ICM does not embed certificate management in phase 1.
- The token is compared on every request except `GET /health`.

## 4. Observability

- `GET /cache` — HTML dashboard of embedding-cache usage (hit rate, disk
  hits, entries, API calls saved). Auto-refreshes every 5s.
- `GET /cache/stats` — the same figures as JSON.
- `GET /health` — unauthenticated liveness probe.
- Server startup logs the bind address; backend is whatever
  `ICM_DB_BACKEND` selected on the central node.

## 5. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| client: `remote store error: … Connection refused` | central node not running / wrong `ICM_REMOTE_URL` / firewall |
| client: `remote store error: … 401` | missing/incorrect `ICM_REMOTE_TOKEN` vs server `--token` |
| `ICM_DB_BACKEND=remote requires ICM_REMOTE_URL` | set `ICM_REMOTE_URL` on the client |
| `embedding dimension N does not match store dimension M` | the embed model/dims changed; run `icm embed --force` on the central node to re-embed |
| memoir/facts/transcript ops fail remotely | ensure the **central** node uses the SQLite backend (Postgres/OpenSearch cover only core memory) |
| hook counters / code-areas empty in remote mode | node-local bookkeeping is not centralized in phase 1 (see plan Manifest); recall/store are unaffected |

## 6. Multi-tenant auth scaffold (F-003 phase-1)

> ### ⚠️ This scaffold resolves IDENTITY, it does NOT isolate DATA
>
> With a `[remote] tokens` map configured, the server maps each Bearer token
> to a **tenant name** and rejects unknown tokens — but **every tenant still
> reads and writes the same shared store**. This layer is **not** a security
> boundary between tenants: it does **not** isolate one tenant's memories
> from another's. Cross-tenant data isolation (Postgres row-level tenant +
> RLS) is **deferred to F-003b** and **not yet implemented**. Do **not**
> deploy this as protection between mutually distrusting tenants.

F-001 ships a single global `--token`. F-003 phase-1 adds an optional
**token → tenant** map so a central node can tell *who* is calling (identity),
as scaffolding for the real isolation work in F-003b.

Configure it on the **central** node's `~/.config/icm/config.toml`:

```toml
[remote.tokens]
"s3cr3t-token-for-team-a" = "team-a"
"s3cr3t-token-for-team-b" = "team-b"
```

Then start the server as usual (the `[remote] tokens` map takes precedence
over `--token` when non-empty):

```bash
icm serve --http 0.0.0.0:11435
```

Behavior:

- **Known token → resolved tenant.** The request proceeds; the server logs
  the **resolved tenant only** — the token is a secret and is **never**
  logged, echoed, or placed in an error message.
- **Unknown or missing token → `401`** (except `GET /health`, always open).
- **No `[remote] tokens` map → F-001 fallback:** the single `--token` guards
  the server and the tenant is reported as `"default"` (fully backward
  compatible; existing single-token deployments are unchanged).

Check the resolved identity:

```bash
curl -s -H "Authorization: Bearer s3cr3t-token-for-team-a" \
  http://<central-host>:11435/whoami
# {"tenant":"team-a"}          # never contains the token
```

Operational note: `[remote] tokens` values are secrets. Restrict read
access to `config.toml` (file permissions are the operator's responsibility);
ICM keeps tokens out of logs and responses but cannot protect the file.

## 7. Supersession — near-duplicate temporal replacement (F-003)

Independently of the transport, storing a new memory now checks the same
topic for an **active near-duplicate** (cosine similarity ≥
`[memory] supersede_threshold`, default `0.90`). If found, the older memory
is marked **superseded** and the new one is stored, preserving temporal
history instead of merging the two in place.

- Superseded memories are **excluded from recall/list/count by default**.
  Pass `--include-superseded` to `icm recall` / `icm list` to see them
  (`icm get <id>` always returns a memory directly, superseded or not).
- Set `supersede_threshold >= 1.0` to **disable** supersession — the store
  then behaves exactly as before (pure dedup), fully backward compatible.
- **Scope:** this is *near-duplicate* detection, not *contradiction*
  detection. "lives in NYC" → "moved to SF" has low surface/vector
  similarity and is **not** caught here; true contradiction resolution
  needs an LLM and is out of phase-1 scope.

## Roadmap (deferred, tracked)

- **F-003a** — extend the Postgres backend to all subsystems (facts, memoir,
  feedback, transcripts), so a central Postgres node is a full replacement
  for SQLite, not just core memory.
- **F-003b** — real cross-tenant **data isolation**: a row-level tenant
  column enforced by Postgres Row-Level Security (`SET LOCAL app.tenant` +
  policies). This is the layer that turns the identity scaffold above into
  an actual security boundary.

## Known limitations (phase 1)

- No built-in TLS (use a tunnel/proxy).
- Single shared token by default; the optional `[remote] tokens` map adds
  **identity only** — it is **not** cross-tenant data isolation (see §6;
  isolation is F-003b).
- Node-local bookkeeping (hook counters/events, code areas, pending
  extraction, pattern mining) is not forwarded to the server; it returns
  `Unsupported` on clients. Cross-machine memory/facts/memoir/feedback/
  transcript sharing is unaffected.
