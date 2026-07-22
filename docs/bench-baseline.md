# ICM performance baseline (F-001)

Machine: Apple Silicon (aarch64-apple-darwin), local dev box. `icm bench`
seeds an **in-memory** store and times store/search throughput (no model
download; embeddings are synthetic 384-dim vectors).

Command: `icm bench --count 5000` (median of 2 runs).

## Requirement 4 — "performance must not degrade"

F-001 adds only **default-OFF** features (`cloud-embeddings`, `remote-store`)
and additive modules. The single change on the default hot path is the
embedding **dimension guard** (one `Vec::len()` comparison per `store` /
`update`) plus one vtable hop from boxing the embedder — both negligible
next to store + FTS + vector work.

### Before / after (dev profile, identical opt level — fair regression check)

Baseline = merge-base `65ae008` (pre-F-001). HEAD = F-001 complete.

| Operation | Baseline (µs/op) | HEAD (µs/op) | Δ |
|---|---|---|---|
| Store (no embeddings) | 65.8 | 62.9 | −4.4% (faster) |
| Store (with embeddings) | 100.5 | 94.0 | −6.5% (faster) |
| FTS5 search | 78.7 | 71.9 | −8.6% (faster) |
| Vector search (KNN), ms/op | 4.05 | 3.95 | within noise |
| Hybrid search, ms/op | 4.85 | 4.75 | within noise |

HEAD is **equal-or-faster than baseline on every metric** — no regression
(differences are run-to-run noise; the dim guard is not measurable).
Well within the ≤ 8% gate.

### Release archive (HEAD, `--release`, reference figures)

| Operation | HEAD release |
|---|---|
| Store (no embeddings) | 24.1 µs/op |
| Store (with embeddings) | 34.1 µs/op |
| FTS5 search | 29.0 µs/op |
| Vector search (KNN) | 1.1 ms/op |
| Hybrid search | 1.2 ms/op |
| Decay (batch, 5000) | 5.3 ms |

### New-path notes (remote / cloud)

- **Remote store** latency is dominated by one HTTP round-trip per call to
  the central node; it is a network path, not a local-store regression,
  and does not affect the default (local SQLite) build measured above.
- **Cloud embed** replaces local model inference with an HTTP call; the
  `CachingEmbedder` (disk + LRU) serves repeats with zero API cost — see
  `/cache` on the server for live hit-rate.

## How to re-run

```bash
cargo build --release -p icm-cli
./target/release/icm bench --count 5000
```
