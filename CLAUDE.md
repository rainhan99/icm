# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Projet: ICM (Infinite Context Memory)

Mémoire persistante pour agents IA, écrite en Rust. Un **seul binaire** (`icm`), zéro
service externe requis en configuration par défaut. Expose un CLI, un serveur MCP
(JSON-RPC 2.0 sur stdio), des hooks pour agents (Claude Code / Codex / Gemini /
Copilot), un dashboard web optionnel, et de la synchro cloud.

Préférences utilisateur :
- Utiliser la commande **`rtk` CLI** pour économiser les tokens (dépôts sur https://github.com/rtk-ai).
- Utiliser **vox** pour un résumé vocal à la fin des tâches.

## Protocole mémoire ICM — OBLIGATOIRE

Ce projet se sert de lui-même. Utilise activement la mémoire ICM.

Les tools MCP ICM sont **deferred** : sans `ToolSearch` préalable ils n'existent pas.
Si le serveur MCP n'est pas connecté dans la session, utilise le binaire `icm` en CLI
(voir `AGENTS.md`, bloc `<!-- icm:start -->`, qui est la source de vérité des triggers).

Séquence en début de session :
```
1. ToolSearch "select:mcp__icm__icm_wake_up,mcp__icm__icm_memory_recall,mcp__icm__icm_memory_store,mcp__icm__icm_memory_forget"
2. icm_wake_up (project="icm")   # ou en CLI : icm wake-up
```

Déclencher un **store** IMMÉDIATEMENT (avant de répondre) quand :

| Événement | topic | importance |
|-----------|-------|------------|
| Erreur résolue | `errors-resolved` | high |
| Décision d'architecture/technique | `decisions-icm` | high |
| Préférence / correction utilisateur | `preferences` | critical |
| Tâche significative terminée | `context-icm` | high |
| +20 tool calls sans store | résumé de progression | medium |

Déclencher un **recall** avant : de proposer une solution, de traiter un bug/erreur
mentionné, ou de résumer le contexte projet.

Ne pas stocker : contenu lisible dans le code, historique git, état éphémère de session.

## Commandes de développement

```bash
# Build (dev / release — le binaire release est fortement optimisé : LTO, panic=abort, strip)
cargo build
cargo build --release

# Gates CI (dans l'ordre où la CI les exécute — fmt → clippy → test → audit)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # warnings = erreurs
cargo test --workspace

# Un seul test
cargo test -p icm-store nom_du_test
cargo test -p icm-cli   -- --nocapture nom_du_test

# Lancer le binaire en dev
cargo run -p icm-cli -- recall "query"
cargo run -p icm-cli -- serve            # serveur MCP stdio

# Audit de sécurité (comme la CI)
cargo audit
```

Le workspace utilise `resolver = "2"`. La CI teste sur Linux / macOS / Windows.

### Feature flags (importants)

Le binaire publié embarque **tous** les backends et choisit à l'exécution. Pour builder
un sous-ensemble, désactive les defaults :

- `embeddings` — modèle fastembed local (téléchargé au 1er run). Sans lui : recherche
  keyword/FTS uniquement. Désactivable au runtime via `--no-embeddings` ou config.
- `backend-sqlite` (défaut) — SQLite in-process (`rusqlite` + `sqlite-vec`), zéro service.
- `postgres` / `opensearch` — backends **additifs** réseau (partage entre replicas).
  Backend actif choisi au **runtime** via `ICM_DB_BACKEND` (`sqlite`|`postgres`|`opensearch`),
  jamais à la compilation. Voir `docs/postgres-backend.md`, `docs/opensearch-backend.md`.
- `tui` — interface `ratatui`. `http-api` — serveur HTTP persistant. `web` — dashboard SPA.
- `vendored-openssl` — requis par les cross-builds du pipeline release (ne pas retirer).

Exemple build léger : `cargo build --no-default-features --features "embeddings,tui,http-api,backend-sqlite"`

## Architecture

Workspace de 4 crates compilant en un binaire `icm`. Graphe de dépendances :
`icm-cli → {icm-core, icm-store, icm-mcp}` ; `icm-mcp → {icm-core, icm-store}` ;
`icm-store → icm-core`. `icm-core` n'a **aucune** I/O.

### Point crucial : tout le store est SYNCHRONE

Malgré l'ancienne spec, le trait `MemoryStore` et les backends sont **synchrones** (pas
d'`async_trait`, pas de tokio dans le chemin de données). Les backends réseau utilisent
des clients bloquants (`postgres`, `ureq`) pour coller à cette surface synchrone.
`tokio` n'est tiré que par les features `http-api` / `web` (serveur axum). N'introduis
pas d'async dans `icm-core`/`icm-store` sans raison forte.

### icm-core — types & traits (`crates/icm-core/`)

Fondation, sans DB. Erreurs typées `IcmError` / `IcmResult` (`thiserror`). Au-delà de
`Memory`/`Importance`/`MemorySource`, plusieurs sous-systèmes cohabitent, chacun avec
son type + trait store :

- **memory** (`Memory`, trait `MemoryStore`) — mémoires semi-structurées, decay temporel,
  recherche hybride, consolidation. `store.rs` porte aussi la déduplication
  (`find_similar_memory`, `DEDUP_SIMILARITY_THRESHOLD = 0.85`).
- **facts** (`Fact`, `FactsStore`) — faits exacts `(entity, key, value)` versionnés,
  distincts du recall sémantique.
- **feedback** (`Feedback`, `FeedbackStore`) — corrections de prédictions IA (FTS5).
- **memoir** (`Memoir`, `Concept`, `MemoirStore`) — couche de connaissance permanente
  (concepts liés, relations).
- **transcript** (`Session`, `Message`, `TranscriptStore`) — sessions/messages verbatim
  (replay), alimentés par le hook d'auto-archive.
- **wake_up** / **context_snapshot** — construction des packs de contexte injectés en
  system-prompt (formats compacts type TOON pour l'injection LLM).
- **auto_link** — liage automatique entre mémoires. **learn** — `icm learn`.
- **embedder** (trait `Embedder`) + **fastembed_embedder** (feature `embeddings`),
  `DEFAULT_EMBEDDING_DIMS = 384`.

### icm-store — persistance (`crates/icm-store/`)

Store enfichable **additif** (issue #301). `Store` (enum, `backend.rs`) dispatch au
runtime vers le backend actif (`BackendKind`). Un `compile_error!` garantit qu'au moins
un backend est compilé. `common.rs` = types de lignes agnostiques ; `schema.rs`/`store.rs`
= SQLite (FTS5 + sqlite-vec) ; `postgres.rs` (pgvector) ; `opensearch.rs` (BM25 + knn).

### icm-mcp — serveur MCP (`crates/icm-mcp/`)

`protocol.rs` = JSON-RPC 2.0, `server.rs` = boucle stdio, `tools.rs` = ~35 tools
(`icm_memory_*`, `icm_memoir_*`, `icm_feedback_*`, `icm_transcript_*`, `icm_learn`,
`icm_wake_up`). Dispatch central dans `tools.rs` (`match tool_name`). Les args peuvent
arriver imbriqués sous `arguments` (variations Claude Code 1.x/2.x, Codex, Gemini).

### icm-cli — binaire (`crates/icm-cli/`)

Point d'entrée `main.rs` (~10k lignes, enum `Commands` clap). Au-delà du CRUD mémoire :
- `serve` — MCP stdio (défaut) ; `--web` (dashboard) ou `--http` (API persistante warm).
- `init` / `doctor` / `uninstall` — intègre/diagnostique/retire ICM des outils IA détectés
  (injecte hooks, skills, entrées `mcpServers`). `install_manifest.rs` trace ce qui est écrit.
- `hook` — hooks agent : `hook post` (PostToolUse, auto-extraction toutes les N tool calls),
  SessionStart (injection wake-up). Alimente `code_areas`, transcripts, extraction différée.
- `extract` / `extract_semantic` / `extract-pending` — extraction de faits (règles → LLM
  optionnel via provider `claude|codex|gemini|ollama`).
- `import` (Claude.ai / ChatGPT / Claude Code / Slack / texte), `cloud` (sync), `upgrade`,
  `archive`, `bench`, `tui`.

### Config & chemins

`config.rs` : ordre de lookup `$ICM_CONFIG` → `~/.config/icm/config.toml` → defaults
intégrés (tout est optionnel). Voir `config/default.toml` pour les sections
(`store`, `memory`, `embeddings`, `extraction`, `recall`, `wakeup`, `consolidate`, `mcp`,
`web`, `cloud`, `archive`). DB : `--db` > `ICM_DB` > data dir plateforme. `ICM_READONLY=1`
ou `--read-only` pour les environnements sandboxés (écritures refusées proprement).

## Contraintes

- **Pas d'`unwrap()`/`expect()`** en code de prod — `thiserror` (`IcmError`) pour les
  erreurs typées de bibliothèque, `anyhow` uniquement dans `icm-cli`.
- `cargo clippy -- -D warnings` doit passer : **tout warning est une erreur**.
- Ajouter des tests unitaires par module + tests d'intégration pour le store
  (`#[cfg(test)]`, `tempfile` pour les DB éphémères).
- Toute nouvelle dépendance déclenche un audit supply-chain en CI (voir `ci.yml`).

## Git & release

- Flux : feature branch → **`develop`** → **`main`**. Ne pas committer/pusher directement
  sur `main` (ni `develop`) — passer par une branche + PR.
- **Conventional commits** obligatoires (`feat:`, `fix:`, `feat!:`…) : ils pilotent le
  versioning. `major=0` → breaking = bump *minor*, feat/fix = bump *patch*.
- `main` : release-please ouvre les PRs de release et taggue `icm-v*` (+ tag `latest`).
- `develop` : pré-releases auto `icm-dev-v*-rc.N` (workflow `cd.yml`), puis back-merge
  automatique de `main` vers `develop` après chaque release stable.
- Terminer les messages de commit par :
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
