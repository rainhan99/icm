# Code graph (F-002)

ICM builds a persistent, incremental **code graph** — symbols and the
calls/references between them — so an agent answers structural questions
("who calls X", "what does X call", "what breaks if I change X") from a
pre-built index in a single `icm_code_explore` call, instead of grep/read
crawling. This is the same idea as
[codegraph](https://github.com/colbymchenry/codegraph), reimplemented
natively in ICM's Rust (no external service): it reuses ICM's SQLite +
FTS5 + MCP + PostToolUse hook + remote store.

## Why (token savings)

Answering "who calls `foo` and what breaks if I change it" the old way is
a grep → open file → read → grep again loop: dozens of tool calls and
many file reads, each spending tokens re-discovering structure. With the
code graph it is **one** `icm_code_explore` call returning the definition,
callers, callees, and blast radius — no file reads. codegraph's published
benchmark across 7 repos measured **−69% tokens / −60% cost**, with tool
calls dropping from a median of 40 to 2–3 and file reads to 0. ICM's
design mirrors that single-call structural answer.

### Before / after (measured on this repo)

Indexed ICM itself: **87 files, 2619 symbols, 21386 refs** in one
`icm code index`. Then:

| Question: "who calls `find_similar_memory` and what's the blast radius?" | Tool calls | File reads |
|---|---|---|
| grep + read crawling | ~10–40 | several |
| `icm code explore find_similar_memory` (or MCP `icm_code_explore`) | 1 | 0 |

The single call returned: definition (`crates/icm-core/src/store.rs:11`),
**2 callers across crates** (`cmd_store` in icm-cli, `tool_store` in
icm-mcp — cross-crate resolution), 2 callees, a 7-symbol transitive blast
radius, and the verbatim source — no file reads.

## Supported languages

Rust, TypeScript/JavaScript (incl. TSX/JSX), Python, Go — one tree-sitter
grammar each. The language registry is pluggable; adding a language is
adding a grammar + a small extractor.

## Usage

```bash
# Index the repository (respects .gitignore; skips node_modules/target/…)
icm code index                 # full index of the current directory
icm code index --incremental   # only re-parse files whose content changed

# Query
icm code explore <symbol>      # definition + callers + callees + blast radius + source
icm code callers <symbol>      # direct callers
icm code impact <symbol>       # transitive blast radius
icm code stats                 # symbols / refs / files / stale counts, by language
```

The MCP tool `icm_code_explore { symbol, max_depth? }` exposes the same
single-call answer to agents.

## Incremental updates

The PostToolUse hook flags edited files **stale** (cheap, one UPDATE) as
you work. `icm code index --incremental` then re-parses only the changed
files (content-hash compared) and clears their stale flag. `icm code
stats` shows the stale count.

## Resolution precision (authorized limitation)

References are resolved with lexical scope + import resolution, plus a
best-effort enclosing-type preference for methods on static languages
(Rust/Go). Full type inference is **not** performed, so dynamic dispatch /
duck typing on dynamic languages (Python/TS/JS) is left `unresolved`
rather than mis-linked. This is an accepted phase-1 boundary (see the
plan's Logic Completeness Manifest).

## Remote sharing (three-tier)

The code graph rides the same remote store as F-001. In remote mode
(`ICM_DB_BACKEND=remote`), the **client parses locally** (tree-sitter is
light; files live on the client) and pushes symbols/edges to the central
node per file via `code.index_file`; queries (`explore`/`callers`/…) run
on the central node and return in a single RPC. Keys are
**repository-relative** paths, so machines sharing a project must use the
same relative layout.

```bash
# central node (owns the graph)
ICM_DB_BACKEND=sqlite icm serve --http 0.0.0.0:11435 --token "$T"
# any dev machine
ICM_DB_BACKEND=remote ICM_REMOTE_URL=http://central:11435 ICM_REMOTE_TOKEN=$T \
  icm code index          # parses locally, stores centrally
```

## Build / stripping

`code-graph` is a **default-on** cargo feature. To build without it (no
tree-sitter, smaller binary):

```bash
cargo build --no-default-features --features backend-sqlite
```

## Limitations (phase 1)

- No framework-aware routing, no cross-language bridging (Swift-ObjC / RN).
- No native file-watcher daemon — incremental relies on the PostToolUse
  hook + `icm code index --incremental`.
- Cross-machine graphs assume the same relative repo layout (no
  cross-commit graph merge).
- Full type inference is out of scope (see resolution precision above).
