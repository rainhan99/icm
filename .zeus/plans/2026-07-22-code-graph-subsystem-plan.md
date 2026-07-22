# Plan — 代码图子系统 (F-002)

- **Spec:** [.zeus/specs/2026-07-22-code-graph-subsystem-design.md](../specs/2026-07-22-code-graph-subsystem-design.md)
- **Feature:** F-002
- **Date:** 2026-07-22
- **Status:** Draft — 待用户签名批准

---

## 1. Header

**Goal:** 原生 Rust 在 ICM 内建代码图:tree-sitter 解析 Rust/TS-JS/Python/Go → 符号+调用边存
SQLite → agent 单次 `icm_code_explore` 查定义/callers/callees/blast-radius,取代 grep/read 省 token。
默认构建零回归、可裁剪、可走 F-001 远程共享。

**Architecture(已定稿决策):**
- 存储:独立 `cg_files/cg_symbols/cg_refs` 表(与 memoir 解耦)。
- 语言:Rust/TS-JS/Python/Go,可插拔 registry。
- 解析:词法作用域 + import 解析;静态语言(Rust/Go)尽量加类型推断消歧,动态语言(Py/TS/JS)
  尽力而为、动态派发标 `unresolved`,**不做全类型推断**(已授权边界,见 Manifest)。
- 打包:`code-graph` feature 默认开,`--no-default-features` 可裁。
- 远程:客户端本地解析 → 按文件 `code.index_file` 推送中心 store;`explore` 服务端一次算完;
  相对路径为键(复用 F-001 `rpc_dispatch`/`RemoteHttpStore`/`Store::Remote`)。

**Tech Stack(活跃):** Rust · SQLite/SQL · HTTP API(远程)· tree-sitter 解析域。

**F-NNN:** F-002。

---

## 2. File Map

### 新建
| 文件 | 职责 |
|---|---|
| `crates/icm-core/src/code_graph.rs` | 类型(`CodeFile`/`Symbol`/`SymbolKind`/`Ref`/`RefKind`/`ExploreResult`/`Blast`)+ `CodeGraphStore` trait + `CodeLanguage` 枚举 |
| `crates/icm-core/src/code_parse/mod.rs` | 解析驱动 + language registry:`parse_file(lang, src) -> (symbols, raw_refs)` |
| `crates/icm-core/src/code_parse/rust.rs` | Rust tree-sitter 查询 + 符号/引用抽取 + 类型消歧规则 |
| `crates/icm-core/src/code_parse/typescript.rs` | TS/JS 抽取(含 tsx) |
| `crates/icm-core/src/code_parse/python.rs` | Python 抽取 |
| `crates/icm-core/src/code_parse/go.rs` | Go 抽取 + 类型消歧 |
| `crates/icm-core/src/code_resolve.rs` | 跨文件词法作用域 + import 解析;静态语言尽力类型消歧 |
| `crates/icm-store/src/code_graph_schema.rs` | `cg_*` 表 + FTS + 迁移 |
| `crates/icm-store/src/code_graph_store.rs` | `impl SqliteStore` 的 CodeGraphStore(upsert/query/delete) |
| `docs/code-graph.md` | 使用/部署/限制/token 证据文档 |

### 修改
| 文件 | 改动 |
|---|---|
| `crates/icm-core/src/lib.rs` | feature-gated 导出;`remote_protocol::ALL_METHODS` 追加 `code.*` |
| `crates/icm-core/src/error.rs` | 新增 `IcmError::CodeGraph(String)` |
| `crates/icm-core/Cargo.toml` | `code-graph` feature + tree-sitter(+4 grammar)可选依赖 |
| `crates/icm-store/src/backend.rs` | `impl CodeGraphStore for Store`(dispatch)+ Remote 臂 |
| `crates/icm-store/src/remote.rs` | `RemoteHttpStore` 的 CodeGraphStore 转发(`code.*` RPC) |
| `crates/icm-store/src/lib.rs` / `Cargo.toml` | 模块声明 + `code-graph` feature 透传 |
| `crates/icm-mcp/src/tools.rs` / `Cargo.toml` | `icm_code_explore` 工具 + 分发 + feature 透传 |
| `crates/icm-cli/src/main.rs` | `code` 子命令(index/explore/callers/impact/stats)+ hook 增量接线 |
| `crates/icm-cli/src/rpc_dispatch.rs` | `code.*` 服务端分发臂 |
| `crates/icm-cli/Cargo.toml` | `code-graph` feature 透传 |
| `config/default.toml` | `[code_graph]` 注释段 |

---

## 3. Architect Risk Analysis(经用户确认)

**Rust 架构师**
- *Restate:* tree-sitter 解析内核(4 grammar)+ 同步 `CodeGraphStore`。
- *Risk:* ①**tree-sitter ABI 对齐**——`tree-sitter` 运行时与 grammar crate 必须同 ABI,错配即崩;在 T1 锁一套协调版本并以 `cargo build` 验证。②grammar 是 C,构建期需 `cc`(CI 具备)。③大仓库**逐文件**流式索引,不整仓入内存。④error node 优雅跳过,禁 unwrap。
- *已答:* 解析精度 = 作用域+import,静态语言尽量类型消歧(决策 A)。

**SQLite/SQL 架构师**
- *Restate:* `cg_*` 表 + FTS;blast-radius 传递闭包。
- *Risk:* ①闭包用**深度上限 BFS**(Rust 内)或递归 CTE,防 N+1/无界。②增量:事务内按文件删+重插;跨文件 ref **按名延迟解析**(存目标名 + 解析出的 symbol_id 可空)避免悬空。③重名/重载消歧靠类型(静态)或标注歧义。

**HTTP API / 远程架构师(SC-9)**
- *Restate:* `code.*` 接入 F-001 转发。
- *Risk:* ①索引**按文件粒度** RPC(`code.index_file(path, hash, symbols, refs)`),不传整仓。②键=**相对路径**,绝不存绝对路径。③`explore`/`callers`/`impact` **服务端一次算完**返回,避免客户端多轮 BFS。④多机并发索引同文件=last-writer-wins。

**tree-sitter 解析域架构师**
- *Restate:* 每语言用 tree-sitter query 抽符号/调用点。
- *Risk:* ①各 grammar 的 node 类型名不同 → 每语言独立 query,共享 `parse_file` 驱动。②动态语言无法静态解析动态派发 → 标 `unresolved`(已授权,非缺陷)。③tsx/jsx 需 typescript grammar 的 tsx 变体。

**用户已确认:** A=作用域+import+静态类型尽力消歧;B=客户端解析按文件推送。

---

## 4. Tasks

> TDD:①失败测试(全码)②跑测确认失败③最小实现④跑测确认通过⑤提交。命令在仓库根执行,
> 前缀 `PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`。

### Stage 0 — 脚手架

#### T1 — `code-graph` feature + tree-sitter 依赖 + `IcmError::CodeGraph`
1. **失败测试**(`error.rs` tests):`assert_eq!(IcmError::CodeGraph("x".into()).to_string(), "code graph error: x");`
2. **确认失败:** `cargo test -p icm-core error::tests::code_graph_error_displays` → `no variant CodeGraph`。
3. **最小实现:** 加 `#[error("code graph error: {0}")] CodeGraph(String)`;icm-core `code-graph = ["dep:tree-sitter","dep:tree-sitter-rust","dep:tree-sitter-typescript","dep:tree-sitter-python","dep:tree-sitter-go"]`;透传到 icm-store/icm-mcp/icm-cli;**锁一套协调 ABI 版本**(候选:`tree-sitter=0.25` + 对应 grammar;实际版本以 `cargo build -p icm-core --features code-graph` 通过为准并记入审计)。icm-cli `default` 追加 `code-graph`。
4. **确认通过:** `cargo test -p icm-core error::tests::code_graph_error_displays` + `cargo build -p icm-core --features code-graph`。
5. **提交:** `feat(core): add IcmError::CodeGraph and code-graph feature + tree-sitter deps`。

#### T2 — code_graph 类型 + `CodeGraphStore` trait + `code.*` 方法名
1. **失败测试**(`code_graph.rs`):`Symbol` serde round-trip;`ALL_METHODS` 含 `"code.index_file"` 且仍唯一。
2. **确认失败:** `cargo test -p icm-core code_graph` → unresolved module。
3. **最小实现:** 定义类型 + `CodeGraphStore` trait(`index_file`/`delete_file`/`get_symbol`/`find_symbols`/`callers`/`callees`/`explore`/`code_stats`/`list_stale`);`ALL_METHODS` 追加 `code.*`;`lib.rs` feature-gated 导出。
4. **确认通过:** `cargo test -p icm-core --features code-graph code_graph`。
5. **提交:** `feat(core): code-graph types, CodeGraphStore trait, code.* rpc names`。

### Stage 1 — 解析内核(SC-1)

#### T3 — Rust 符号抽取
1. **失败测试**(`code_parse/rust.rs`):`parse_file(Rust, "pub fn foo(){} struct Bar;")` → 含 `foo`(Function)、`Bar`(Struct)。
2. **确认失败:** `cargo test -p icm-core --features code-graph code_parse::rust`。
3. **最小实现:** registry + `parse_file` 驱动;Rust tree-sitter query 抽 fn/struct/enum/trait/impl-method/mod。
4. **确认通过:** 同命令。
5. **提交:** `feat(core): tree-sitter Rust symbol extraction`。

#### T4 — TS/JS 符号抽取(含 tsx)
1. **失败测试:** `parse_file(TypeScript, "export function foo(){} class Bar{}")` → `foo`,`Bar`。
2. **确认失败:** `cargo test -p icm-core --features code-graph code_parse::typescript`。
3. **最小实现:** TS + tsx grammar query 抽 function/class/method/arrow-const/export。
4. **确认通过 / 提交:** `feat(core): tree-sitter TS/JS symbol extraction`。

#### T5 — Python 符号抽取
1. **失败测试:** `parse_file(Python, "def foo():\n  pass\nclass Bar:\n  pass")` → `foo`,`Bar`。
2. **确认失败:** `cargo test -p icm-core --features code-graph code_parse::python`。
3. **最小实现:** Python grammar query 抽 def/class/method。
4. **确认通过 / 提交:** `feat(core): tree-sitter Python symbol extraction`。

#### T6 — Go 符号抽取
1. **失败测试:** `parse_file(Go, "package p\nfunc Foo(){}\ntype Bar struct{}")` → `Foo`,`Bar`。
2. **确认失败:** `cargo test -p icm-core --features code-graph code_parse::go`。
3. **最小实现:** Go grammar query 抽 func/method/type/struct/interface。
4. **确认通过 / 提交:** `feat(core): tree-sitter Go symbol extraction`。

### Stage 2 — 引用与解析(SC-2)

#### T7 — 调用/引用边抽取(未解析,按名+位置)
1. **失败测试:** Rust `fn a(){ b(); }` → raw ref `b`(Call)自 `a`。
2. **确认失败:** `cargo test -p icm-core --features code-graph code_parse::refs`。
3. **最小实现:** 每语言 query 抽 call/identifier/import;产出 `Ref{ from_symbol, name, kind, resolved: None }`。
4. **确认通过 / 提交:** `feat(core): extract call/reference edges (unresolved)`。

#### T8 — 作用域+import 解析 + 静态语言类型消歧
1. **失败测试**(`code_resolve.rs`):跨两文件:`a.rs` 调 `helper()`,`b.rs` 定义 `helper` 且被 import → resolved 指向 b 的 symbol;Python 动态 `obj.m()` → `unresolved`。
2. **确认失败:** `cargo test -p icm-core --features code-graph code_resolve`。
3. **最小实现:** 词法作用域(局部/模块/import)解析 ref→symbol_id;Rust/Go 用声明类型对方法调用消歧;动态派发标 unresolved。
4. **确认通过 / 提交:** `feat(core): scope+import resolution with best-effort static typing`。

### Stage 3 — 存储(SC-3/4)

#### T9 — `cg_*` schema + FTS
1. **失败测试**(`code_graph_schema.rs`):init 后 `cg_files`/`cg_symbols`/`cg_refs`/`cg_symbols_fts` 存在。
2. **确认失败:** `cargo test -p icm-store --features code-graph code_graph_schema`。
3. **最小实现:** 建表(symbols: id/file/name/kind/line/span;refs: from_symbol/target_name/target_symbol(nullable)/kind;files: path/hash/lang/stale)+ 索引 + FTS。
4. **确认通过 / 提交:** `feat(store): code_graph schema + FTS`。

#### T10 — SqliteStore CodeGraphStore 实现
1. **失败测试:** `index_file` 写入后 `get_symbol`/`find_symbols`(FTS)/`callers`/`callees` 命中;`delete_file` 清除。
2. **确认失败:** `cargo test -p icm-store --features code-graph code_graph_store`。
3. **最小实现:** 事务化 upsert(先删该文件旧行再插)+ 查询(callers=按 target_symbol 反查,callees=正查)+ 延迟按名解析。
4. **确认通过 / 提交:** `feat(store): SqliteStore CodeGraphStore impl`。

#### T11 — Store enum 分发
1. **失败测试:** `Store::in_memory()` 上经 `CodeGraphStore` trait 调用 `code_stats()` 返回空统计。
2. **确认失败:** `cargo test -p icm-store --features code-graph store_code_graph_dispatch`。
3. **最小实现:** `impl CodeGraphStore for Store` 用 `dispatch!`;Remote 臂先 `Unsupported`(T19 接线)。
4. **确认通过 / 提交:** `feat(store): dispatch CodeGraphStore across Store enum`。

### Stage 4 — 索引 + 增量(SC-5/6)

#### T12 — `icm code index` 全量
1. **失败测试**(icm-cli):对临时 fixture 目录 index 后 `code_stats` 符号数 > 0;`.gitignore`/`node_modules` 被排除。
2. **确认失败:** `cargo test -p icm-cli --features code-graph code_index_full`。
3. **最小实现:** walkdir + 语言探测 + gitignore/排除 + 逐文件 parse→resolve→`index_file`(存 content hash)。
4. **确认通过 / 提交:** `feat(cli): icm code index (full) with exclusions`。

#### T13 — 增量 + hook + staleness
1. **失败测试:** 改一个文件内容(hash 变)→ `index --incremental` 只重解析该文件、符号更新、stale 清除;未变文件跳过。
2. **确认失败:** `cargo test -p icm-cli --features code-graph code_index_incremental`。
3. **最小实现:** hash 比对跳过未变;PostToolUse hook 从 `code_areas` 取改动文件 → 标 stale / 重索引。
4. **确认通过 / 提交:** `feat(cli): incremental code index + hook wiring + staleness`。

### Stage 5 — 查询(SC-7)

#### T14 — explore + blast-radius
1. **失败测试:** 索引 fixture 后 `explore("foo")` 返回定义 + 源码切片 + callers + callees + 有界 blast-radius。
2. **确认失败:** `cargo test -p icm-core --features code-graph explore_blast_radius`(纯逻辑用 in-memory store)。
3. **最小实现:** explore 组合 get_symbol + callers + callees + 深度上限 BFS 传递闭包 + 源码切片。
4. **确认通过 / 提交:** `feat(core): explore with bounded blast-radius`。

#### T15 — callers/impact/stats
1. **失败测试:** `callers("foo")` 精确集合;`code_stats` 计数正确。
2. **确认失败:** `cargo test -p icm-store --features code-graph code_callers_stats`。
3. **最小实现:** callers/impact 查询 + stats 聚合(符号/边/文件/语言/stale 计数)。
4. **确认通过 / 提交:** `feat(store): callers/impact/stats queries`。

### Stage 6 — MCP + CLI(SC-8)

#### T16 — MCP `icm_code_explore`
1. **失败测试**(icm-mcp):dispatch `icm_code_explore {symbol}` 在有图的 store 上返回结构化结果。
2. **确认失败:** `cargo test -p icm-mcp --features code-graph tool_code_explore`。
3. **最小实现:** tools.rs 注册工具 + 分发到 `store.explore`。
4. **确认通过 / 提交:** `feat(mcp): icm_code_explore tool`。

#### T17 — CLI `code explore|callers|impact|stats`
1. **失败测试:** `code stats` 在空图打印 0(smoke via cmd fn 单测)。
2. **确认失败:** `cargo test -p icm-cli --features code-graph code_cli_subcommands`。
3. **最小实现:** `Commands::Code` 子命令 + 各处理函数(TOON/human 输出)。
4. **确认通过 / 提交:** `feat(cli): code explore/callers/impact/stats subcommands`。

### Stage 7 — 远程(SC-9)

#### T18 — 服务端 `code.*` dispatch
1. **失败测试**(icm-cli `rpc_dispatch`):`dispatch(&store, None, "code.explore", {symbol})` 返回结果;未知 `code.x` 报错。
2. **确认失败:** `cargo test -p icm-cli --features "code-graph,remote-store" rpc_dispatch::tests::code_`。
3. **最小实现:** rpc_dispatch 增 `code.index_file`/`code.delete_file`/`code.explore`/`code.callers`/`code.callees`/`code.code_stats`/`code.find_symbols` 臂(explore 服务端算完)。
4. **确认通过 / 提交:** `feat(cli): server-side code.* store-RPC dispatch`。

#### T19 — RemoteHttpStore CodeGraphStore 转发 + Store::Remote 臂
1. **失败测试**(icm-store):进程内 mock/server round-trip:客户端 `index_file` 推送 → `explore` 命中(相对路径键)。
2. **确认失败:** `cargo test -p icm-store --features "code-graph,remote-store" remote_code_graph`。
3. **最小实现:** `impl CodeGraphStore for RemoteHttpStore` 转发 `code.*`;`Store::Remote` 臂改为真实转发;相对路径规范化。
4. **确认通过 / 提交:** `feat(store): RemoteHttpStore forwards CodeGraphStore (code.* over JSON-RPC)`。

### Stage 8 — 打包/观测/证据(SC-10/11/12)

#### T20 — feature 裁剪 + 零回归
1. **验证(可命令):** `cargo build --no-default-features --features backend-sqlite`(**不含 tree-sitter**)通过;`cargo build`(默认含 code-graph)通过;`./target/release/icm bench --count 5000` 与 `docs/bench-baseline.md` 相差 ≤ 8%。
2. **确认失败:** 首跑前无对照。
3. **最小实现:** 确认 feature 门正确(tree-sitter 仅 code-graph 拉入);把 bench 结果追加 `docs/bench-baseline.md`(F-002 段)。
4. **确认通过 / 提交:** `docs/chore: verify code-graph strippable + zero-regression bench`。

#### T21 — 观测 + 文档 + token 证据
1. **验证(可命令):** `test -f docs/code-graph.md && grep -q icm_code_explore docs/code-graph.md`;`icm code stats` 输出计数。
2. **确认失败:** 文档不存在 → grep 失败。
3. **最小实现:** 写 `docs/code-graph.md`(语言/用法/增量/远程限制/token before-after/裁剪);`config/default.toml` 加 `[code_graph]` 注释;采集一次 explore vs grep 的工具调用/文件读取对比归档。
4. **确认通过 / 提交:** `docs: code-graph guide + config + token efficiency evidence`。

---

## 5. Test Plan

**Unit:** 每语言符号抽取(T3-T6)、ref 抽取(T7)、作用域/类型解析(T8)、schema(T9)、
store CRUD/查询(T10/T15)、explore/blast-radius(T14)、error/类型 serde(T1/T2)。
**Integration:** 全量索引 fixture 目录(T12)、增量+hash 跳过(T13)、Store 分发(T11)、
MCP 工具(T16)、CLI 子命令(T17)、服务端 code.* dispatch(T18)。
**E2E:** 对 ICM 自身仓库 `icm code index` → MCP `icm_code_explore <真实函数>` 一次返回
callers+blast-radius(对比 grep 爬取);远程:客户端解析推送 → 另一进程 explore 命中(T19)。
**Regression:** `cargo test --workspace`(默认含 code-graph)全绿证明 memory/facts/... 不受影响;
`--no-default-features --features backend-sqlite` 编译(证明可裁);`icm bench` ≤8%。

---

## 6. Security Review

**静态扫描:** `cargo audit`(新增 tree-sitter + 4 grammar 触发供应链审计,T1 记录版本 + 出处);
`cargo clippy --workspace --all-targets -- -D warnings`。
**威胁模型(新增面):**
- 解析**不可信源码**:tree-sitter 对畸形/超大输入必须不 panic、不 OOM(逐文件 + error node 容错 + 可选大小上限)。C grammar 崩溃风险 → 输入受控为本地文件,tree-sitter 成熟稳定;仍加大小/时间保护。
- 路径:索引遍历限制在目标仓库内,遵循 `.gitignore`;远程只存**相对路径**,防绝对路径泄漏/越界。
- `code.*` RPC:复用 F-001 Bearer 中间件 + 方法白名单(`ALL_METHODS`);`index_file` 负载大小有上限,防滥用。
- 无秘密:代码图不含凭据;源码切片可能含仓库内敏感串 → 与 memory 同等对待(远程走 token 保护的 LAN)。
**具体检查:** 输入校验(语言探测 + 大小上限)、路径遍历(限定根 + gitignore)、RPC 方法白名单、
远程负载上限、panic 纪律(解析全 Result/skip)。

---

## 7. Logic Review Checkpoints

Rust/SQL/远程/解析透镜清单(数据流、错误层级、状态归属、竞态、复杂度、模块边界、YAGNI、可维护性):
- **CP-A(Stage 2 后):** 解析是否全部 Result/跳过、无 unwrap?动态派发是否正确标 unresolved 而非乱指?
- **CP-B(Stage 3 后):** 增量删+重插是否事务化?按名延迟解析是否避免悬空 ref?FTS/索引是否覆盖查询路径?
- **CP-C(Stage 5 后):** blast-radius BFS 是否有深度/总量上限(防爆炸)?explore 是否单次算完?
- **CP-D(Stage 7 后):** 远程是否只存相对路径?explore 是否服务端算完(非客户端多轮)?同步纪律(无 async 入 core/store)?`RemoteHttpStore` 无 unwrap、错误归 `IcmError::Remote`?
以上检查点在对应 Stage 收尾暂停人工过一遍。

---

## 8. G4 contract delta

完成后追加到契约 DoD:
1. `cargo fmt/clippy -D warnings/test --workspace`(默认含 code-graph)全绿。
2. `cargo build --no-default-features --features backend-sqlite` 通过(不含 tree-sitter)。
3. `cargo test --workspace --features "code-graph,remote-store"` 全绿。
4. `icm code index`(ICM 自身)成功;`icm code explore <fn>` 返回 def+callers+callees+源码。
5. 四语言解析单测通过;跨文件解析测试通过。
6. 增量:改文件后符号更新 + stale 清除。
7. 远程 round-trip:客户端解析推送 → explore 命中。
8. 默认 `icm bench` 与基线 ≤8%。
9. token before/after 归档 `docs/code-graph.md`。

---

## 9. Logic Completeness Manifest

**Every requirement in the linked spec MUST be implemented in full.**

**Authorized simplifications:**
- **Simplification:** 调用解析采用「词法作用域 + import + 静态语言(Rust/Go)尽力类型消歧」;动态语言(Python/TS/JS)的动态派发/鸭子类型调用标记为 `unresolved`,**不做全类型推断**。
  - **Reason:** 对动态语言做全类型推断近乎实现类型检查器,一期不可行(codegraph 本身亦用启发式);用户在 brainstorming 确认此可行边界(选「尽量加类型推断」而非全推断)。
  - **Approved by:** rainhan@coupert.com
  - **Approved at:** 2026-07-22
  - **Restoration ticket:** F-002 后续 —「动态语言类型推断增强 / 框架感知路由」。

### Spec Coverage Matrix
| SC-ID | Capability | 实现任务 | 验证命令 |
|---|---|---|---|
| SC-1 | tree-sitter 四语言符号抽取 | T3,T4,T5,T6 | `cargo test -p icm-core --features code-graph code_parse` |
| SC-2 | 引用/调用边 + 作用域/类型解析 | T7,T8 | `cargo test -p icm-core --features code-graph code_resolve` |
| SC-3 | code_graph schema + FTS | T9 | `cargo test -p icm-store --features code-graph code_graph_schema` |
| SC-4 | CodeGraphStore trait + SQLite + 分发 | T2,T10,T11 | `cargo test -p icm-store --features code-graph code_graph_store` |
| SC-5 | icm code index 全量 + 排除 | T12 | `cargo test -p icm-cli --features code-graph code_index_full` |
| SC-6 | 增量 + hook + staleness | T13 | `cargo test -p icm-cli --features code-graph code_index_incremental` |
| SC-7 | explore + callers/impact/blast-radius | T14,T15 | `cargo test -p icm-core --features code-graph explore_blast_radius` |
| SC-8 | MCP icm_code_explore + CLI | T16,T17 | `cargo test -p icm-mcp --features code-graph tool_code_explore` |
| SC-9 | 远程 code.* 转发共享 | T18,T19 | `cargo test -p icm-store --features "code-graph,remote-store" remote_code_graph` |
| SC-10 | feature 默认开可裁 + 零回归 | T1,T20 | `cargo build --no-default-features --features backend-sqlite && ./target/release/icm bench --count 5000` |
| SC-11 | stats 观测 + 文档 | T15,T21 | `icm code stats && grep -q icm_code_explore docs/code-graph.md` |
| SC-12 | token before/after 证据 | T21 | `grep -qiE "before|after|token" docs/code-graph.md` |

**手工覆盖检查声明(降级):** `scripts/check-spec-coverage.sh` 不存在,本矩阵人工核对;SC-1..SC-12 各映射 ≥1 任务,无孤儿。

---

## 10. File Size Constraints

阈值取默认表。

| 文件 | 类型 | 预估行 | 判定 |
|---|---|---|---|
| `code_graph.rs` | 类型+trait(relaxed) | ~220 | OK |
| `code_parse/mod.rs` | 驱动 | ~150 | OK |
| `code_parse/{rust,typescript,python,go}.rs` | 各语言 query+抽取 | ~180-260 each | OK |
| `code_resolve.rs` | 复杂逻辑 | ~380 | OK(逼近则按「作用域/类型/import」拆子模块) |
| `code_graph_schema.rs` | schema(relaxed) | ~160 | OK |
| `code_graph_store.rs` | service | ~360 | OK(逼近则按 query/mutate 拆) |
| `docs/code-graph.md` | 文档(relaxed) | ~200 | OK |
| `backend.rs`(改) | 路由(relaxed) | +40 | OK |
| `remote.rs`(改) | service | +120 → ~700 | (relaxed);逼近则把 CodeGraphStore 转发拆 `remote/code_graph.rs` |
| `rpc_dispatch.rs`(改) | service match | +90 → ~500 | (relaxed);>550 则拆 `rpc_dispatch/code.rs` |
| `tools.rs`(改) | MCP 注册(relaxed) | +60 | OK |
| `main.rs`(改) | **既有 ~10.9k,预存 OVER** | +180 | 既有超标;`code` 子命令逻辑尽量抽到 code_parse/store,main 仅接线 |

无新建文件越界;`code_resolve.rs`/`code_graph_store.rs`/`remote.rs`/`rpc_dispatch.rs` 设拆分预案。

---

**User-approved:** 2026-07-22 by rainhan@coupert.com
