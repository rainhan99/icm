# Spec — 三层共享 Memory 架构(云端 embed + 远程 HTTP store + embedding 缓存可视化)

- **Feature:** F-001
- **Date:** 2026-07-21
- **Status:** Draft (待用户批准)
- **Brainstorm mode:** [1] 逐题走读
- **Contract anchored:** `CLAUDE.md` + `AGENTS.md`

---

## Goal / Scope

### 背景与目标

让**多台开发机共享同一个项目的 memory**,同时**节省本地资源**(客户端零模型下载、零本地数据库)。采用三层架构:

```
开发机A (icm 轻客户端) ┐
开发机B (icm 轻客户端) ┼─HTTP─► 中心 icm serve --http ──► Postgres(pgvector) 或 SQLite
开发机C (icm 轻客户端) ┘         │  持有 warm embedder / 云端 OpenAI 兼容 embed
                                 │  embedding 结果缓存(磁盘持久 + 内存 LRU 热层)
                                 └─ /cache/stats + 自包含 HTML 缓存看板
```

**关键架构洞察:** 远程模式下客户端**无 embedder**。`RemoteStore` 向服务端转发**文本**(查询/记忆),embedding 与缓存全部在**服务端**发生;`MemoryStore::search_hybrid(query, embedding, …)` 中的 `embedding` 参数在远程模式被忽略,改由服务端对 `query` 现场计算。这是"客户端零模型"得以成立的机制。

**服务端 → PG 这一段今天已由现有 `postgres` 后端 + `ICM_DB_BACKEND=postgres` 实现,本 spec 不重复建设**;真正新增的是「客户端 → 服务端」这一段。

### Scope Checklist

- **SC-1** — 新增 OpenAI 兼容云端 embedder(`OpenAiEmbedder` 实现 `Embedder` trait,用阻塞 `ureq` 命中 `POST {base_url}/embeddings`,支持批量数组),置于新 cargo feature `cloud-embeddings` 后,默认关闭。
- **SC-2** — embedder 运行时选择:`init_embedder` 依据 `[embeddings] provider`(`local` | `openai`)分发到本地 `FastEmbedder` 或 `OpenAiEmbedder`;默认 `local`,行为与今日一致。
- **SC-3** — embedding 结果缓存 `CachingEmbedder`(装饰器包裹任意 `Embedder`):磁盘持久层(key = `hash(model + normalized_text)`,存于现有 `cache_dir()/embeddings/`)+ 内存 LRU 热层;跨进程重启保留。
- **SC-4** — 缓存指标(原子计数,非加锁):`hits` / `misses` / `entries` / `disk_bytes` / `estimated_api_calls_saved`,可被 HTTP 层读取。
- **SC-5** — `icm serve --http` 的 axum 服务端 API 从 6 端点**扩展到覆盖全部 5 个 store trait**(Memory / Facts / Memoir / Feedback / Transcript)的完整 CRUD + 检索方法;请求/响应形状复用 `recall_format` 与现有 handler 约定。
- **SC-6** — `RemoteHttpStore` 客户端:实现全部 5 个 store trait,方法体通过阻塞 `ureq` 转发到远程服务端;远程模式发送**文本**,不在客户端做 embedding。
- **SC-7** — 新增 `Store::Remote(RemoteHttpStore)` 变体 + `BackendKind::Remote`,由 `ICM_DB_BACKEND=remote` + `ICM_REMOTE_URL`(+ 可选 `ICM_REMOTE_TOKEN`)在运行时选中;对 **CLI / MCP stdio server / PostToolUse hook 三者透明**生效。
- **SC-8** — embedding 维度一致性守卫:配置声明 embed 维度;启动与写入路径检测,若与库内既有向量维度不符则**明确报错**并提示 `icm embed --force` 重建,绝不静默产生混维向量。
- **SC-9** — 缓存可视化:服务端新增 `GET /cache/stats`(JSON)+ `GET /cache`(Rust 内联、自包含、无构建步骤的轻量 HTML 看板页),置于 `http-api` feature 内(不依赖缺失的 SvelteKit SPA 源码)。
- **SC-10** — 性能门:默认构建 `icm bench` 与基线**零回归**(8% 噪声带内);并为远程/云端新路径采集一次基准数据归档到 `docs/`。
- **SC-11** — cargo feature 与文档:`cloud-embeddings` 与 `remote-store` 两个新 feature **加性、默认关闭**;`http-api` 默认路径不变;新增三层部署文档 `docs/remote-backend.md`,补 `config/default.toml` 注释。

### In scope
- 上述 SC-1..SC-11。
- 远程模式对三入口(CLI/MCP/hook)透明。
- 内网 LAN 部署姿态。

### Out of scope(第一期不做,记录为已知限制)
- 内置 TLS / 证书管理 —— 加密交给 SSH 隧道 / Tailscale / 反向代理。
- 多用户各自 token、按 project 的服务端鉴权隔离(单一共享 token 即可)。
- 切换 embed 模型时的自动全库 re-embed(改为守卫 + 手动 `icm embed --force`)。
- 补建 SvelteKit SPA 缓存页(改用自包含 HTML)。
- 服务端 → PG 段的新建(已存在)。

### 用户 / 角色
- **主用户:** 在多台电脑上开发同一项目的开发者(即本项目作者)。
- **消费方:** 每台开发机上的 `icm` CLI、Claude Code 的 MCP stdio server、PostToolUse hook。

### 边界与角落场景
- 客户端在远程模式下**禁用本地 embeddings**(无模型);语义检索由服务端完成。
- 服务端不可达 / 网络抖动:远程调用需有清晰错误与超时,不得 panic(契约:无 `unwrap`)。
- 云端 embed API 失败:降级路径需与现有 FTS fallback 一致或明确报错。
- 维度冲突(本地 384/768 ↔ OpenAI 1536):SC-8 守卫。
- 高频 PostToolUse hook 走远程:注意每次网络往返成本(性能门 SC-10 覆盖;可后续加本地节流,本期不做)。

---

## Architecture / Context dependencies

**复用的既有模式(不可偏离):**
- `icm-store::backend::Store` enum + `dispatch!` 宏(运行时后端分发,SurrealDB `Any` 风格)。新增 `Remote` 变体接入同一 enum。
- `icm-core::Embedder` trait(同步、`Send + Sync`)。`OpenAiEmbedder` 与 `CachingEmbedder` 都实现它。
- `crates/icm-cli/src/http_api.rs` 的 axum handler、`OutputFormat`(TOON/JSON)协商、Bearer 中间件——SC-5 在其上扩展。
- `crates/icm-cli/src/cloud.rs` 已示范的阻塞 `ureq` + serde 客户端模式——SC-1/SC-6 沿用。
- `icm-store::store::SqliteStore` 已用 `lru::LruCache`——SC-3 内存热层沿用同一 crate。
- 现有 Bearer token 鉴权(`http_api.rs` auth middleware)——SC-7 客户端携带,服务端复用。

**关键约束(来自契约):**
- **store 层保持同步**;`tokio` 只经 `http-api`/`web` feature。`RemoteHttpStore` 用**阻塞** `ureq`,契合同步 trait,不引入 async 到 core/store。
- **禁止 `unwrap()`/`expect()`** 生产代码;库 crate 用 `thiserror`(`IcmError`),CLI 用 `anyhow`。新增 `IcmError` 变体承载远程/HTTP/embed 错误。
- 新 feature **加性、默认关闭**,默认精简构建不受影响。

**跨 crate 影响面:**
- `icm-core`:`OpenAiEmbedder`(feature `cloud-embeddings`)、`CachingEmbedder`、缓存指标类型、可能新增 `Embedder::dimensions` 一致性辅助。
- `icm-store`:`RemoteHttpStore`(feature `remote-store`)、`Store::Remote`、`BackendKind::Remote`、维度守卫。
- `icm-cli`:`init_embedder` 分发、`http_api.rs` 全量 CRUD 扩展 + `/cache` 页、feature 连线、配置字段、部署文档。
- `icm-mcp`:无需改动(经 `Store` enum 透明);仅需确认远程模式下的构造路径。

---

## Environment requirements

- **无新增第三方依赖**(预期):`ureq`、`serde_json`、`lru`、`sha2`、`chrono` 均已在 workspace。若实现中确需新依赖,触发 CI 供应链审计并在 plan 中列出。
- **新 cargo features(加性,默认关闭):**
  - `cloud-embeddings`(icm-core → icm-cli 透传)—— 编入 `OpenAiEmbedder`。
  - `remote-store`(icm-store → icm-mcp/icm-cli 透传)—— 编入 `RemoteHttpStore` + `Store::Remote`。
  - 发布二进制沿用"全部编入、运行时选择"策略(与 postgres/opensearch 一致)。
- **运行时开关:**
  - embed:`[embeddings] provider="openai" | "local"`,`base_url`(默认 `https://api.openai.com/v1`),`model`,`dimensions`;API key 走 env `ICM_EMBED_API_KEY`(回退 `OPENAI_API_KEY`)。
  - 远程 store:`ICM_DB_BACKEND=remote` + `ICM_REMOTE_URL` + 可选 `ICM_REMOTE_TOKEN`。
  - 缓存:`cache_dir()/embeddings/` 磁盘目录;内存 LRU 容量可配。
- **安全姿态(第一期):** 内网 LAN,明文 HTTP + 可选 Bearer token(防误连);TLS 交隧道/反代。
- **CI:** 云端 embed 单测**不得**在 CI 打真实 API(无凭据)——用 mock/条件跳过。远程 round-trip 测试用**进程内 axum + 客户端**(复用 `http_api` 既有测试手法)。

---

## Definition of Done delta

将追加到项目契约的完成判据(G4):

1. `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿(现有门,不放松)。
2. **默认构建零回归**:`cargo build`(默认 feature)后 `icm bench` 结果与归档基线相差 ≤ 8%。
3. `cargo build --no-default-features --features backend-sqlite`(最精简)仍编译通过——证明新 feature 加性、可裁剪。
4. `--features cloud-embeddings,remote-store` 构建通过且新增测试全绿。
5. 远程 round-trip 集成测试:进程内启动服务端 + `RemoteHttpStore` 客户端,对 5 个 trait 的核心方法做存取往返断言。
6. 维度守卫测试:维度不符时返回明确 `IcmError` 而非 panic 或静默。
7. `GET /cache/stats` 返回 hits/misses/entries;`GET /cache` 返回可渲染 HTML(状态码 200 + `text/html`)。
8. 远程/云端新路径的一次性基准数据已归档到 `docs/`。

---

## Handoff state requirements

- **可观测性:**
  - 服务端启动日志打印:激活后端(`BackendKind`)、embedder 种类与维度、缓存目录与命中率初值。
  - `GET /cache/stats`(JSON)+ `GET /cache`(HTML 看板)常驻观测入口。
- **交接文档:** `docs/remote-backend.md` —— 三层部署步骤、env/config 示例、LAN 安全说明、故障排查(服务端不可达、维度冲突、缓存失效)。
- **给下一会话的状态:** feature flag 矩阵、配置键清单、"远程模式=客户端零 embedder"这一不变量需写入文档,避免后人误在客户端启用本地模型。

---

## 7-gate impact map

| Gate | 新约束 |
|---|---|
| **G1 代码** | 4 crate 均有改动;`RemoteHttpStore` 全 5-trait 转发是主工作量。 |
| **G2 TDD** | 云端 embed(mock)、缓存命中/未命中、维度守卫、远程 round-trip 均需先写失败测试。 |
| **G3 验证** | 每 SC 有对应 `cargo test` / `cargo build --features …` / `curl` 验证命令。 |
| **G4 DoD** | 追加上节 8 条判据;默认构建零回归为硬门。 |
| **G5 E2E** | 端到端:客户端 `icm store`(远程)→ 服务端 embed+缓存+落库 → 另一客户端 `icm recall`(远程)命中。 |
| **G6 评审** | 两阶段评审关注:同步性未被破坏、无 `unwrap`、feature 加性、无静默降级。 |
| **G7 交接** | `docs/remote-backend.md` + 观测入口 + 不变量记录到位方可闭合。 |
