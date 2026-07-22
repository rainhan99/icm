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

## Known limitations (phase 1)

- No built-in TLS (use a tunnel/proxy).
- Single shared token (no per-user auth / per-project isolation).
- Node-local bookkeeping (hook counters/events, code areas, pending
  extraction, pattern mining) is not forwarded to the server; it returns
  `Unsupported` on clients. Cross-machine memory/facts/memoir/feedback/
  transcript sharing is unaffected.
