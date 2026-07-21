# Plan — 三层共享 Memory 架构 (F-001)

- **Spec:** [.zeus/specs/2026-07-21-three-tier-shared-memory-design.md](../specs/2026-07-21-three-tier-shared-memory-design.md)
- **Feature:** F-001
- **Date:** 2026-07-21
- **Status:** Draft — 待用户签名批准

---

## 1. Header

**Goal:** 让多台开发机通过「轻客户端 → 中心 icm serve → SQLite」三层共享同一项目 memory,客户端零模型零本地库;新增 OpenAI 兼容云端 embed、embedding 结果缓存及其可视化;默认构建性能零回归。

**Architecture(已定稿):**
- 转发机制 = **JSON-RPC over HTTP**(决策A)。服务端在现有 axum(`http_api.rs`)上挂 `POST /rpc`,dispatch 到本地 `Store`;客户端 `RemoteHttpStore` 用阻塞 `ureq` 发 JSON-RPC。
- 中心服务端后端 = **SQLite**(决策B),唯一支持全 5 store trait。
- 共享 RPC 契约放 **icm-core**(避免 icm-store→icm-mcp 循环依赖);复用 JSON-RPC 2.0 envelope *模式*,而非 icm-mcp 的具体代码。
- 远程模式客户端 **禁用本地 embedder**,转发文本,服务端 embed —— 这是"节省本地资源"的机制不变量。

**Tech Stack(活跃):** Rust(唯一语言)· HTTP API · SQLite/SQL · 极简静态 HTML。

**F-NNN:** F-001。

---

## 2. File Map

### 新建
| 文件 | 职责(一行) |
|---|---|
| `crates/icm-core/src/remote_protocol.rs` | 共享 store-RPC 契约:JSON-RPC envelope + 方法名常量 + 参数/结果 serde 形状 |
| `crates/icm-core/src/openai_embedder.rs` | `OpenAiEmbedder`(feature `cloud-embeddings`)—— ureq 命中 `/embeddings`,支持批量 |
| `crates/icm-core/src/caching_embedder.rs` | `CachingEmbedder` + `CacheMetrics`(磁盘持久 + 内存 LRU 热层,原子指标) |
| `crates/icm-store/src/remote.rs` | `RemoteHttpStore`(feature `remote-store`)+ 转发宏,实现全 5 trait |
| `crates/icm-cli/src/rpc_dispatch.rs` | 服务端 store-RPC dispatcher:方法名 → 本地 `Store` 调用,generic serde |
| `docs/remote-backend.md` | 三层部署文档(env/config 示例、LAN 安全、故障排查) |

### 修改
| 文件 | 改动 |
|---|---|
| `crates/icm-core/src/error.rs` | 新增 `IcmError::Remote(String)` 传输错误变体 |
| `crates/icm-core/src/lib.rs` | feature-gated 导出 `remote_protocol` / `openai_embedder` / `caching_embedder` |
| `crates/icm-core/Cargo.toml` | 新增 `cloud-embeddings` feature(可选依赖 `ureq`/`sha2`/`lru`,均已在 workspace) |
| `crates/icm-store/src/backend.rs` | `BackendKind::Remote` + `Store::Remote` 变体 + dispatch 臂 + 从 env 构造 |
| `crates/icm-store/src/lib.rs` | feature-gated 导出 `RemoteHttpStore` |
| `crates/icm-store/Cargo.toml` | 新增 `remote-store` feature(可选依赖 `ureq`,已在 workspace) |
| `crates/icm-cli/src/http_api.rs` | 新增 `POST /rpc`、`GET /cache/stats`、`GET /cache`;接线 dispatcher + Bearer |
| `crates/icm-cli/src/main.rs` | `init_embedder` provider 分发;`open_store` 远程接线;远程模式禁用本地 embed |
| `crates/icm-cli/src/config.rs` | `EmbeddingsConfig` 增 `provider`/`base_url`/`dimensions`;远程配置读取 |
| `crates/icm-cli/Cargo.toml` | `cloud-embeddings`、`remote-store` 透传 feature |
| `crates/icm-mcp/Cargo.toml` | `remote-store` 透传 feature(供 `cargo test -p icm-mcp`) |
| `config/default.toml` | 新配置键注释 |

---

## 3. Architect Risk Analysis(经用户确认)

**Rust 架构师**
- *Restate:* 新增 3 个 Embedder/Store 类型,全部同步 + 阻塞 `ureq`,不引入 async 到 core/store。
- *Risk:* `RemoteHttpStore` 需实现 ~67 个 trait 方法;用**转发宏**(`method!(name => rpc_method_name)`)压成薄壳,避免样板膨胀与服务端漂移。panic 纪律:所有网络/序列化错误走 `IcmError::Remote`,零 `unwrap`。
- *已解决:* 决策A(JSON-RPC)消除 ~67 REST 端点的维护面。

**HTTP API 架构师**
- *Restate:* `http_api.rs` 加一个 `POST /rpc` 承载全部 store 操作 + `/cache` 观测端点。
- *Risk:* ①错误语义 —— `get()`/`get_fact()` 的 `Ok(None)` 必须序列化为 JSON `null` 结果(而非 JSON-RPC error),客户端反序列化回 `Ok(None)`;②方法允许清单 —— dispatcher 只接受注册过的方法名,未知方法返回 `-32601`,不 panic;③幂等 —— `store`/`set_fact` 语义与本地一致(dedup 由服务端 Store 负责,客户端无额外逻辑)。
- *Question 已答:* 转发机制 = JSON-RPC(决策A)。

**SQLite/SQL 架构师**
- *Restate:* 中心服务端 `ICM_DB_BACKEND=sqlite`,持有唯一权威库。
- *Risk:* ①并发 —— 多客户端并发命中服务端,axum handler 经 `Arc<Mutex<Store>>` 串行化 DB 访问(与现有 `http_api`/`web` 一致),SQLite WAL 下读写安全;②维度 —— SC-8 复用已存在的 `Store::read_stored_embedding_dims`,写入/启动时校验云端维度(如 1536)与库内一致,不符则 `IcmError`。
- *已解决:* 决策B(SQLite)使全 5 trait 端到端可用(PG 会对 4 个子系统返回 `Unsupported`)。

**前端(极简)架构师**
- *Restate:* `/cache` 为 Rust 内联自包含 HTML,无框架无构建。
- *Risk:* 只读展示;数据经 `/cache/stats` 拉取;无用户输入回显 → 无 XSS 注入面;经现有 Bearer 中间件。CSP 由静态内容天然收敛。
- *已解决:* 规避缺失的 SvelteKit 源码。

**用户已确认的架构决策:** A=JSON-RPC over HTTP;B=服务端 SQLite。

---

## 4. Tasks

> 每个任务遵循 TDD:①写失败测试(全码)②跑测试确认失败(精确命令 + 期望)③写最小实现④跑测试确认通过⑤提交。所有 `cargo test` 命令在仓库根执行。

### Stage 0 — 脚手架与契约

#### T1 — `IcmError::Remote` + 两个加性 feature

1. **失败测试**(`crates/icm-core/src/error.rs` 末尾 `#[cfg(test)]`):
```rust
#[test]
fn remote_error_displays() {
    let e = super::IcmError::Remote("connection refused".into());
    assert_eq!(e.to_string(), "remote store error: connection refused");
}
```
2. **确认失败:** `cargo test -p icm-core error::tests::remote_error_displays` → 期望编译错误 `no variant named Remote`。
3. **最小实现:** 在 `IcmError` 加 `#[error("remote store error: {0}")] Remote(String)`;在三个 `Cargo.toml` 加 feature:
   - icm-core:`cloud-embeddings = ["dep:ureq", "dep:sha2", "dep:lru"]`(三者 workspace 已有,声明为 optional)。
   - icm-store:`remote-store = ["dep:ureq"]`。
   - icm-cli:`cloud-embeddings = ["icm-core/cloud-embeddings"]`、`remote-store = ["icm-store/remote-store", "icm-mcp/remote-store"]`;icm-mcp:`remote-store = ["icm-store/remote-store"]`。
4. **确认通过:** `cargo test -p icm-core error::tests::remote_error_displays`。
5. **提交:** `git commit -am "feat(core): add IcmError::Remote and cloud-embeddings/remote-store features"`。

#### T2 — store-RPC 共享契约 (`remote_protocol.rs`)

1. **失败测试**(新文件内):
```rust
#[test]
fn envelope_roundtrips() {
    let req = RpcRequest { method: "memory.count".into(), params: serde_json::json!({}) };
    let bytes = serde_json::to_vec(&req).unwrap();
    let back: RpcRequest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.method, "memory.count");
}
#[test]
fn method_names_are_unique() {
    let all = super::ALL_METHODS;
    let mut seen = std::collections::HashSet::new();
    for m in all { assert!(seen.insert(*m), "duplicate rpc method: {m}"); }
}
```
2. **确认失败:** `cargo test -p icm-core remote_protocol` → 期望 `unresolved module`。
3. **最小实现:** 定义 `RpcRequest{method:String, params:Value}`、`RpcResponse{result:Option<Value>, error:Option<String>}`(复用 JSON-RPC 2.0 形状),以及 `pub const ALL_METHODS: &[&str]`(全 67 方法的稳定命名,如 `"memory.search_hybrid"`, `"facts.set_fact"`, `"memoir.add_link"`, `"transcript.record_message"` …)。在 `lib.rs` 导出(gate 在 `remote-store` 或 `cloud-embeddings` 任一;实际用 `#[cfg(any(feature="remote-store", ...))]` —— 简化为无 gate 的纯类型模块,零依赖)。
4. **确认通过:** `cargo test -p icm-core remote_protocol`。
5. **提交:** `git commit -am "feat(core): add shared store-RPC protocol contract"`。

### Stage 1 — SC-1/SC-2 云端 embedder

#### T3 — `OpenAiEmbedder`

1. **失败测试**(`openai_embedder.rs`,用进程内 mock:起一个返回固定 JSON 的 `tiny_http`/手写 TcpListener,或注入一个 `base_url` 指向本地测试服务器):
```rust
#[test]
fn embeds_via_openai_shape() {
    let server = mock_embeddings_server(vec![vec![0.1_f32; 4]]); // 返回 {"data":[{"embedding":[...]}]}
    let e = OpenAiEmbedder::new(&server.base_url(), "test-key", "text-embedding-3-small", 4);
    let v = e.embed("hello").unwrap();
    assert_eq!(v.len(), 4);
    assert_eq!(e.dimensions(), 4);
}
```
2. **确认失败:** `cargo test -p icm-core --features cloud-embeddings openai_embedder` → 期望 `cannot find type OpenAiEmbedder`。
3. **最小实现:** `OpenAiEmbedder{base_url, api_key, model, dims}`,`embed`/`embed_batch` POST `{base_url}/embeddings` body `{"model":..,"input":[..]}`,解析 `data[].embedding`;`dimensions()→dims`;API key 绝不进日志。错误 → `IcmError::Embedding`。
4. **确认通过:** `cargo test -p icm-core --features cloud-embeddings openai_embedder`。
5. **提交:** `git commit -am "feat(core): OpenAI-compatible cloud embedder behind cloud-embeddings"`。

#### T4 — `init_embedder` provider 分发

1. **失败测试**(`main.rs` 测试模块):
```rust
#[test]
fn provider_openai_selects_cloud() {
    let kind = resolve_embedder_kind("openai", "https://x/v1", "m", 1536);
    assert!(matches!(kind, EmbedderKind::OpenAi{dims:1536, ..}));
    assert!(matches!(resolve_embedder_kind("local","","m",0), EmbedderKind::Local));
}
```
2. **确认失败:** `cargo test -p icm-cli provider_openai_selects_cloud`。
3. **最小实现:** `EmbeddingsConfig` 加 `provider`(默认 `"local"`)、`base_url`、`dimensions`;`resolve_embedder_kind` + `init_embedder` 依 provider 构造 `FastEmbedder` 或 `OpenAiEmbedder`(后者 gate `cloud-embeddings`,未编入时报明确 `IcmError::Config`)。
4. **确认通过:** `cargo test -p icm-cli provider_openai_selects_cloud`。
5. **提交:** `git commit -am "feat(cli): select embedder provider (local|openai) at runtime"`。

### Stage 2 — SC-3/SC-4 缓存 embedder + 指标

#### T5 — `CachingEmbedder` 内存 LRU + 指标

1. **失败测试**(`caching_embedder.rs`,用一个记数的 stub Embedder):
```rust
#[test]
fn second_embed_hits_cache() {
    let inner = CountingEmbedder::new(vec![1.0,2.0]);
    let c = CachingEmbedder::in_memory(Box::new(inner.clone()), "modelX", 8);
    let _ = c.embed("foo").unwrap();
    let _ = c.embed("foo").unwrap();
    assert_eq!(inner.calls(), 1);              // 底层只调一次
    assert_eq!(c.metrics().hits(), 1);
    assert_eq!(c.metrics().misses(), 1);
}
```
2. **确认失败:** `cargo test -p icm-core --features cloud-embeddings caching_embedder::tests::second_embed_hits_cache`。
3. **最小实现:** `CacheMetrics{hits,misses,entries: AtomicU64}`;`CachingEmbedder{inner, lru: Mutex<LruCache<String,Vec<f32>>>, metrics, model}`;key = `hash(model + normalize(text))`;`embed` 查 LRU → 命中记 hit,未命中调 inner 记 miss 并写入。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(core): in-memory caching embedder with atomic metrics"`。

#### T6 — 磁盘持久层

1. **失败测试:**
```rust
#[test]
fn disk_cache_survives_new_instance() {
    let dir = tempfile::tempdir().unwrap();
    let inner = CountingEmbedder::new(vec![3.0]);
    { let c = CachingEmbedder::with_dir(Box::new(inner.clone()), "mX", 8, dir.path()); c.embed("bar").unwrap(); }
    let inner2 = CountingEmbedder::new(vec![3.0]);
    let c2 = CachingEmbedder::with_dir(Box::new(inner2.clone()), "mX", 8, dir.path());
    let _ = c2.embed("bar").unwrap();
    assert_eq!(inner2.calls(), 0);             // 新实例读磁盘,底层零调用
    assert_eq!(c2.metrics().disk_hits(), 1);
}
```
2. **确认失败:** `cargo test -p icm-core --features cloud-embeddings caching_embedder::tests::disk_cache_survives_new_instance`。
3. **最小实现:** 磁盘目录 `dir/<hashprefix>/<hash>.bin`(f32 小端);`with_dir` 优先查内存→磁盘→inner;写入两层。默认目录 `cache_dir()/embeddings/`。指标加 `disk_bytes`/`disk_hits`。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(core): disk-persistent embedding cache layer"`。

### Stage 3 — SC-8 维度守卫

#### T7 — 维度一致性守卫

1. **失败测试**(`crates/icm-store` 集成测试,用现成 SqliteStore):
```rust
#[test]
fn mismatched_dims_are_rejected_not_silent() {
    let s = SqliteStore::with_dims(":memory:", 384).unwrap();
    let mut m = Memory::new("t".into(), "c".into(), Importance::Medium);
    m.embedding = Some(vec![0.0; 1536]);        // 维度不符
    let err = s.store(m).unwrap_err();
    assert!(matches!(err, IcmError::InvalidInput(_)));
}
```
2. **确认失败:** `cargo test -p icm-store mismatched_dims_are_rejected_not_silent`。
3. **最小实现:** 在写入路径(`store`/`update`)校验 `embedding.len()` 与库声明维度(复用 `read_stored_embedding_dims`/`with_dims`);不符 → `IcmError::InvalidInput("embedding dim N != store dim M; run `icm embed --force`")`。启动时 CLI 若 provider 维度与库不符,打印同类错误。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(store): reject embedding dimension mismatch instead of silent corruption"`。

### Stage 4 — SC-5 服务端 JSON-RPC dispatch

#### T8 — dispatcher: MemoryStore(20 方法)

1. **失败测试**(`rpc_dispatch.rs`):
```rust
#[test]
fn dispatch_memory_count() {
    let store = Store::in_memory().unwrap();
    let resp = dispatch(&store, None, "memory.count", serde_json::json!({}));
    assert_eq!(resp.result.unwrap(), serde_json::json!(0));
}
#[test]
fn dispatch_unknown_method_errors() {
    let store = Store::in_memory().unwrap();
    let resp = dispatch(&store, None, "bogus.method", serde_json::json!({}));
    assert!(resp.error.is_some());
}
```
2. **确认失败:** `cargo test -p icm-cli --features remote-store rpc_dispatch::tests`。
3. **最小实现:** `fn dispatch(store, embedder, method, params) -> RpcResponse`,`match method`:每个 `memory.*` 反序列化 params → 调 `store.<m>()` → 序列化。`get()`→`Ok(None)` 映射为 `result: null`。`search_hybrid`/`store` 在此处对文本做**服务端 embedding**(用传入 embedder)。未知方法 → `error`。IcmError → `error` 字符串(NotFound 除外,见错误语义)。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(cli): server-side store-RPC dispatch for MemoryStore"`。

#### T9 — dispatcher: Facts(6) + Feedback(6)

1. **失败测试:** round-trip `facts.set_fact` 后 `facts.get_fact` 取回同值;`feedback.feedback_stats` 返回 0。(全码同 T8 模式,针对这两 trait。)
2. **确认失败:** `cargo test -p icm-cli --features remote-store rpc_dispatch::tests::facts_feedback_roundtrip`。
3. **最小实现:** 在 `dispatch` 的 `match` 增 `facts.*`(6)、`feedback.*`(6)臂。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(cli): store-RPC dispatch for Facts and Feedback"`。

#### T10 — dispatcher: Memoir(26)

1. **失败测试:** `memoir.create_memoir` → `memoir.get_memoir` 取回;`memoir.add_concept`+`memoir.add_link` → `memoir.get_links_for_memoir` 返回 1 条。(全码。)
2. **确认失败:** `cargo test -p icm-cli --features remote-store rpc_dispatch::tests::memoir_roundtrip`。
3. **最小实现:** 增全部 26 个 `memoir.*` 臂。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(cli): store-RPC dispatch for Memoir graph"`。

#### T11 — dispatcher: Transcript(9)

1. **失败测试:** `transcript.ensure_session` → `transcript.record_message` → `transcript.list_session_messages` 返回 1 条。(全码。)
2. **确认失败:** `cargo test -p icm-cli --features remote-store rpc_dispatch::tests::transcript_roundtrip`。
3. **最小实现:** 增全部 9 个 `transcript.*` 臂。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(cli): store-RPC dispatch for Transcript"`。

#### T12 — `POST /rpc` axum handler + auth + 错误映射

1. **失败测试**(`http_api.rs` 测试,进程内起 server + reqwest/ureq client 或直接调 handler):
```rust
#[tokio::test]
async fn rpc_endpoint_roundtrips_store_and_recall() {
    let app = build_router(AppState::in_memory_with_embedder());
    // POST /rpc memory.store {topic,summary,...} → 得到 id
    // POST /rpc memory.count → 1
    // Bearer 缺失且配置了 token → 401
}
```
2. **确认失败:** `cargo test -p icm-cli --features "http-api,remote-store" rpc_endpoint_roundtrips_store_and_recall`。
3. **最小实现:** `POST /rpc` handler 解析 `RpcRequest` → `rpc_dispatch::dispatch(&store_lock, embedder, method, params)` → `RpcResponse` JSON;复用现有 Bearer 中间件(`/rpc` 受保护)。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(cli): expose store-RPC over HTTP at POST /rpc"`。

### Stage 5 — SC-6/SC-7 客户端 + Store::Remote + 运行时选择

#### T13 — `RemoteHttpStore` + 转发宏:实现 MemoryStore

1. **失败测试**(`crates/icm-store` 集成测试,进程内起 T12 的 server,或用 `httpmock`/手写 listener):
```rust
#[test]
fn remote_store_memory_roundtrip() {
    let srv = spawn_test_rpc_server();                 // 内含 SQLite Store + embedder
    let r = RemoteHttpStore::new(&srv.url(), None);
    let id = r.store(Memory::new("t".into(),"hello world".into(),Importance::Medium)).unwrap();
    assert!(!id.is_empty());
    assert_eq!(r.count().unwrap(), 1);
    let hits = r.search_hybrid("hello", &[], 5).unwrap(); // 空 embedding:服务端 embed
    assert_eq!(hits.len(), 1);
}
```
2. **确认失败:** `cargo test -p icm-store --features remote-store remote_store_memory_roundtrip`。
3. **最小实现:** `RemoteHttpStore{base_url, token, agent: ureq::Agent}`;转发宏 `rpc!(fn_name(args) -> Ret => "memory.method")` 生成薄壳:序列化 params → POST `/rpc` → 反序列化 `result`;error → `IcmError::Remote`。实现 `MemoryStore` 全 20 方法。`search_hybrid` 把 `query` 放进 params,忽略 `embedding` 参数(服务端 embed)。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(store): RemoteHttpStore forwarding MemoryStore over JSON-RPC"`。

#### T14 — `RemoteHttpStore`:Facts + Feedback + Memoir + Transcript(53 方法)

1. **失败测试:** 每 trait 一个 round-trip(经真实进程内 server):facts set/get、feedback store/stats、memoir create/get+link、transcript ensure/record/list。(全码,4 个测试。)
2. **确认失败:** `cargo test -p icm-store --features remote-store remote_store_other_traits`。
3. **最小实现:** 用同一 `rpc!` 宏实现其余 4 个 trait 的 53 方法。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(store): RemoteHttpStore forwards Facts/Feedback/Memoir/Transcript"`。

#### T15 — `BackendKind::Remote` + `Store::Remote` + env 构造

1. **失败测试**(`backend.rs`):
```rust
#[test]
fn remote_backend_from_env() {
    std::env::set_var("ICM_DB_BACKEND", "remote");
    assert_eq!(BackendKind::from_env().unwrap(), BackendKind::Remote);
    std::env::remove_var("ICM_DB_BACKEND");
}
```
2. **确认失败:** `cargo test -p icm-store --features remote-store remote_backend_from_env`。
3. **最小实现:** `BackendKind::Remote`(`from_env` 认 `"remote"`);`Store::Remote(RemoteHttpStore)`(gate `remote-store`);`dispatch!` 宏加 `Remote` 臂;`Store::new`/`with_dims` 在 Remote 时读 `ICM_REMOTE_URL`(必需,缺失→`IcmError::Config`)+ `ICM_REMOTE_TOKEN`(可选),忽略 path。
4. **确认通过:** 同上命令 + `cargo build -p icm-store --features remote-store`。
5. **提交:** `git commit -am "feat(store): Store::Remote runtime backend via ICM_DB_BACKEND=remote"`。

#### T16 — CLI/MCP/hook 透明 + 远程禁本地 embed

1. **失败测试**(`main.rs`):
```rust
#[test]
fn remote_mode_disables_local_embedder() {
    // 当 backend=remote 时,embeddings_enabled 应被强制为 false
    assert!(!compute_embeddings_enabled(/*cfg on*/true, /*cli no_emb*/false, /*backend*/BackendKind::Remote));
    assert!(compute_embeddings_enabled(true, false, BackendKind::Sqlite));
}
```
2. **确认失败:** `cargo test -p icm-cli --features remote-store remote_mode_disables_local_embedder`。
3. **最小实现:** `open_store` 经 `Store::from_env`/`new` 已透明返回 Remote(CLI/MCP `serve`/hook 三入口都走 `open_store`)。`compute_embeddings_enabled` 在 Remote 时返回 false(客户端不加载模型)。MCP `run_server`、`hook post` 无需改 —— 它们接收 `&Store`。
4. **确认通过:** 同上命令 + 手动 E2E(见 Test Plan E2E)。
5. **提交:** `git commit -am "feat(cli): transparent remote mode across CLI/MCP/hook; disable local embedder"`。

### Stage 6 — SC-9 缓存可视化

#### T17 — `GET /cache/stats`

1. **失败测试:** 起 server(embedder = CachingEmbedder,预热一次 embed)→ `GET /cache/stats` → JSON 含 `hits`/`misses`/`entries`。
2. **确认失败:** `cargo test -p icm-cli --features http-api cache_stats_endpoint`。
3. **最小实现:** `AppState` 持 `Option<Arc<CacheMetrics>>`(从 CachingEmbedder 取);`GET /cache/stats` 返回原子快照 JSON。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(cli): GET /cache/stats endpoint"`。

#### T18 — `GET /cache` 自包含 HTML

1. **失败测试:** `GET /cache` → 200 + `content-type: text/html` + body 含 `"hits"`/`"misses"` 标签文本。
2. **确认失败:** `cargo test -p icm-cli --features http-api cache_html_page`。
3. **最小实现:** 内联 `const CACHE_HTML: &str`(纯 HTML + 一段 fetch `/cache/stats` 渲染,或服务端注入初值),无外链无框架。
4. **确认通过:** 同上命令。
5. **提交:** `git commit -am "feat(cli): self-contained /cache HTML dashboard"`。

### Stage 7 — SC-10/SC-11 性能 + 文档

#### T19 — 性能门:默认构建基准归档

1. **失败测试(可验证脚本):** 无单测;验证命令 = `cargo build --release && ./target/release/icm bench --count 1000`,把结果记入 `docs/bench-baseline.md`;新增前后对比,默认路径差异 ≤ 8%。
2. **确认失败:** 首次运行无基线文件 → 视为待建立。
3. **最小实现:** 采集默认构建基线 + 远程/云端新路径一次性数据,写入 `docs/bench-baseline.md`。
4. **确认通过:** `./target/release/icm bench --count 1000` 输出在噪声带内(人工核对归档)。
5. **提交:** `git commit -am "docs: archive icm bench baseline for perf-gate"`。

#### T20 — 部署文档 + feature 可裁剪验证

1. **失败测试(可验证):** `test -f docs/remote-backend.md && grep -q ICM_REMOTE_URL docs/remote-backend.md`;且 `cargo build --no-default-features --features backend-sqlite`(最精简)编译通过。
2. **确认失败:** 文档不存在 → grep 失败。
3. **最小实现:** 写 `docs/remote-backend.md`(三层部署、env/config、LAN 安全、故障排查、"远程=客户端零 embedder"不变量);补 `config/default.toml` 注释。
4. **确认通过:** 上述 grep + `cargo build --no-default-features --features backend-sqlite`。
5. **提交:** `git commit -am "docs: three-tier remote deployment guide + config comments"`。

---

## 5. Test Plan

**Unit(按文件):**
- `error.rs`:Remote display(T1)。
- `remote_protocol.rs`:envelope round-trip、方法名唯一(T2)。
- `openai_embedder.rs`:embed/embed_batch/dimensions,mock server(T3)。
- `caching_embedder.rs`:内存命中、磁盘跨实例命中、指标计数、key 含 model 防串(T5/T6)。
- `main.rs`:provider 分发(T4)、远程禁 embed(T16)。
- `backend.rs`:Remote from_env(T15)。

**Integration(跨组件):**
- `rpc_dispatch`:5 trait 各自 round-trip(T8-T11)。
- `POST /rpc`:store+recall+401(T12)。
- `RemoteHttpStore` ↔ 进程内 server:5 trait round-trip(T13/T14)。
- 维度守卫(T7)。
- `/cache/stats`、`/cache`(T17/T18)。

**E2E(真实用户路径):** 终端脚本:①`ICM_DB_BACKEND=sqlite icm serve --http 127.0.0.1:11500 --token T`(后台,SQLite + provider=openai 或 local)②另一 shell `ICM_DB_BACKEND=remote ICM_REMOTE_URL=http://127.0.0.1:11500 ICM_REMOTE_TOKEN=T icm store -t proj -c "远程写入验证"` ③`... icm recall "远程写入" ` 命中 ④`curl -s localhost:11500/cache/stats` 显示 hits/misses。

**Regression(既有保护):** `cargo test --workspace`(默认 feature)全绿,证明默认路径(本地 SQLite + 本地 embed + 现有 6 端点 http_api + MCP)行为不变;`cargo test --workspace --features "cloud-embeddings,remote-store"` 全绿。

---

## 6. Security Review

**静态扫描:** `cargo audit`(CVE)、`cargo clippy --workspace --all-targets -- -D warnings`;任何新增依赖触发 CI 供应链审计(本计划预期零新增 workspace 依赖,仅把已有 crate 声明为 optional)。

**威胁模型(新增攻击面):**
- `POST /rpc` 接收任意 JSON-RPC:**方法允许清单**(仅 `ALL_METHODS`),未知方法 `-32601`,不 eval、不反射任意调用;params 反序列化失败 → 结构化 error,不 panic。
- 明文 HTTP over LAN(第一期决策):**Bearer token 防误连**;文档明示非 localhost 建议配 token + 隧道加密。无内置 TLS(记为限制)。
- 云端 embed 出站:`base_url` 由**运营者**配置(非用户输入)→ SSRF 风险低;仍在文档提示只填可信端点。
- **Secrets:** API key(`ICM_EMBED_API_KEY`/`OPENAI_API_KEY`)与 `ICM_REMOTE_TOKEN` **绝不进日志/错误串**(复用 web.rs 既有"不打印密码"纪律)。

**具体检查项:** ①输入校验(RPC params 反序列化 + 方法白名单)②密钥不落日志 ③auth(Bearer 中间件覆盖 `/rpc` 与 `/cache*`,`/health` 例外)④注入(无 SQL 字符串拼接;经既有参数化 Store)⑤路径遍历(磁盘缓存 key = 定长 hash,非用户路径)⑥隐私(memory 文本发往云端 embed —— feature 默认关 + 文档告知)。

---

## 7. Logic Review Checkpoints

**Rust 透镜清单(每检查点回答):** 数据流向、错误处理层级、状态归属、竞态、复杂度阈值、模块边界、YAGNI、6 个月可维护性。

- **CP-A(Stage 2 后,缓存):** cache key 是否含 model(防跨模型串向量)?磁盘写是否原子(临时文件 + rename)?指标用原子非锁?
- **CP-B(Stage 4 后,服务端 dispatch):** 错误语义 —— `Ok(None)` 是否序列化为 `null` 而非 error?未知方法是否安全拒绝?embedding 是否只在服务端发生?
- **CP-C(Stage 5 后,客户端 + 选择):** `RemoteHttpStore` 是否零 `unwrap`?网络错误是否全归 `IcmError::Remote`?同步纪律 —— core/store 未引入 async?`search_hybrid` 是否正确忽略客户端 embedding 参数?三入口(CLI/MCP/hook)是否都透明走 Remote?
- **CP-D(Stage 6 后):** `/cache` 是否无用户输入回显(无 XSS)?

以上检查点在对应 Stage 收尾任务处**暂停执行**,人工过一遍再继续。

---

## 8. G4 contract delta

完成后追加到项目契约 `## Contraintes`/DoD(或 `.zeus/dod.md`):

1. `cargo fmt/clippy -D warnings/test --workspace` 全绿(维持)。
2. 默认构建 `icm bench` 与 `docs/bench-baseline.md` 相差 ≤ 8%。
3. `cargo build --no-default-features --features backend-sqlite` 编译通过(feature 加性可裁剪)。
4. `cargo test --workspace --features "cloud-embeddings,remote-store"` 全绿。
5. 远程 5-trait round-trip 集成测试存在且通过。
6. 维度冲突返回 `IcmError` 而非 panic/静默。
7. `GET /cache/stats` 与 `GET /cache` 正常响应。
8. 远程/云端新路径基准数据已归档。

---

## 9. Logic Completeness Manifest

**Every requirement in the linked spec MUST be implemented in full. Authorized simplifications: (none).**

> 说明:spec `## Out of scope`(内置 TLS、多用户鉴权、自动 re-embed、补建 SPA)是**已批准的范围边界**,非对范围内 SC 的简化,故不计入本 Manifest 的简化条目。

### Spec Coverage Matrix

| SC-ID | Capability | 实现任务 | 验证命令 |
|---|---|---|---|
| SC-1 | OpenAI 兼容云端 embedder | T3 | `cargo test -p icm-core --features cloud-embeddings openai_embedder` |
| SC-2 | embedder provider 运行时分发 | T4 | `cargo test -p icm-cli provider_openai_selects_cloud` |
| SC-3 | 磁盘+LRU 缓存 embedder | T5, T6 | `cargo test -p icm-core --features cloud-embeddings caching_embedder` |
| SC-4 | 缓存原子指标 | T5, T17 | `cargo test -p icm-cli --features http-api cache_stats_endpoint` |
| SC-5 | 服务端全 5-trait JSON-RPC dispatch | T8, T9, T10, T11, T12 | `cargo test -p icm-cli --features "http-api,remote-store" rpc_` |
| SC-6 | RemoteHttpStore 客户端(全 5 trait) | T13, T14 | `cargo test -p icm-store --features remote-store remote_store_` |
| SC-7 | Store::Remote 运行时选择 + 三入口透明 | T15, T16 | `cargo test -p icm-cli --features remote-store remote_mode_disables_local_embedder` |
| SC-8 | 维度冲突守卫 | T7 | `cargo test -p icm-store mismatched_dims_are_rejected_not_silent` |
| SC-9 | /cache/stats + /cache HTML | T17, T18 | `cargo test -p icm-cli --features http-api cache_html_page` |
| SC-10 | 默认零回归 + 新路径基准归档 | T19 | `./target/release/icm bench --count 1000`(对照 docs/bench-baseline.md) |
| SC-11 | 加性 feature + 部署文档 | T1, T20 | `cargo build --no-default-features --features backend-sqlite && grep -q ICM_REMOTE_URL docs/remote-backend.md` |

**手工覆盖检查声明(降级):** `scripts/check-spec-coverage.sh` 不存在,故本矩阵为**人工核对**;已逐条确认 SC-1..SC-11 各映射到 ≥1 任务,无孤儿。

---

## 10. File Size Constraints

阈值取 writing-plans 默认表(项目契约无显式 file-size 表)。

| 文件 | 类型 | 预估行数 | 判定 |
|---|---|---|---|
| `remote_protocol.rs` | 类型/常量(relaxed) | ~140 | OK |
| `openai_embedder.rs` | service | ~160 | OK |
| `caching_embedder.rs` | 复杂逻辑 | ~260 | OK |
| `remote.rs` | service(宏生成 67 薄壳) | ~380 | OK(宏压缩;逼近则按 trait 拆子模块) |
| `rpc_dispatch.rs` | service(67 match 臂) | ~420 | (relaxed, 高体量低复杂度 match);>500 则按 trait 拆 `rpc_dispatch/{memory,memoir,...}.rs` |
| `docs/remote-backend.md` | 文档(relaxed) | ~200 | OK |
| `error.rs`(改) | 类型 | +5 | OK |
| `backend.rs`(改) | 路由(relaxed) | +40 | OK |
| `config.rs`(改) | config(relaxed) | +20 | OK |
| `http_api.rs`(改) | controller | 733 → ~900 | (relaxed, controller);/rpc 逻辑已抽到 `rpc_dispatch.rs` 控制膨胀 |
| `main.rs`(改) | **既有 10743 行,预存 OVER** | +60 | 既有超标,非本计划引入;改动为外科式,新逻辑抽到 `rpc_dispatch.rs`/embedder 模块,不加剧 |

无新建文件越界。`rpc_dispatch.rs` / `remote.rs` 设有拆分预案(按 trait 分子模块),逼近阈值即触发。

---

**User-approved:** 2026-07-21T10:29:59Z by rainhan@coupert.com
