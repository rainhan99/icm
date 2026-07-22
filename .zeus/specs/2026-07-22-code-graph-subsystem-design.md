# Spec — 代码图子系统(F-002,对标 codegraph,原生融入 ICM)

- **Feature:** F-002
- **Date:** 2026-07-22
- **Status:** Draft(待用户批准)
- **Brainstorm mode:** [1] 逐题走读
- **Contract anchored:** `CLAUDE.md` + `AGENTS.md`
- **参考(仅设计,不复制源码):** github.com/colbymchenry/codegraph

---

## Goal / Scope

### 背景与目标

让 agent 像 codegraph 一样,从 ICM 预建的**代码图索引**里一次调用查到符号定义、
调用链、影响半径(callers/callees/blast-radius),**取代 grep/read 反复爬取**,大幅省
token(codegraph 基准:token −69%、文件读取降到 0、工具调用中位 40→2-3)。原生用
ICM 的 Rust 重写,**不外挂 codegraph 服务、不引入其源码**;技术栈天然契合(ICM 已是
Rust + SQLite + FTS5 + MCP + PostToolUse hook + F-001 远程 store)。

### 关键决策(用户已拍板)
- **A 存储**:独立 `code_graph` 新表(与 memoir 永久知识图解耦)。
- **B 语言**:一期 Rust + TS/JS + Python + Go(4 个 tree-sitter grammar),架构可插拔。
- **C 打包**:独立 `code-graph` feature,**默认开**,`--no-default-features` 可裁掉。
- **D 远程共享**:一期即支持代码图走 F-001 远程 store 跨机共享。

### 关键不变量(远程模式)
代码文件在各机本地,故 **tree-sitter 解析在客户端本地进行**(轻量,区别于 F-001 卸载
的重 embedding 模型),产出的符号/边行推送到中心 store;查询转发到中心。跨机以**相对
仓库路径**为键(假设各机相同相对布局)。

### Scope Checklist
- **SC-1** — tree-sitter 解析内核:抽取符号(函数/方法/类/结构体/接口/枚举等)覆盖
  Rust、TS/JS、Python、Go;可插拔 language registry,加语言=加 grammar。
- **SC-2** — 抽取引用/调用边 + import,并做跨文件解析(import→定义、调用点→被调定义)。
- **SC-3** — `code_graph` SQLite schema:`cg_files` / `cg_symbols` / `cg_refs`(调用/引用边)
  + 符号名 FTS5;含 per-file 内容 hash 供增量判断。
- **SC-4** — `CodeGraphStore` trait 于 icm-core;SQLite 实现于 icm-store;接入 `Store` enum
  与 `dispatch!`(新增变体臂)。
- **SC-5** — `icm code index`:全量 + 增量索引;排除 `.gitignore` / `node_modules` / 构建产物。
- **SC-6** — 增量同步:复用 PostToolUse hook 的 `code_areas`(已跟踪改动文件),改动后重解析
  对应文件并打 staleness 标记。
- **SC-7** — 查询能力:`explore(symbol)` → 逐字源码 + 定义位置 + callers + callees +
  影响半径(传递闭包,带深度上限);`callers` / `impact` 查询。
- **SC-8** — MCP 工具 `icm_code_explore`(单次调用给出结构化答案)+ CLI `icm code
  explore|callers|impact|stats`。
- **SC-9** — 远程共享:`code.*` RPC 方法接入 F-001 的 `rpc_dispatch` + `RemoteHttpStore`
  转发;`CodeGraphStore` 覆盖 `Store::Remote`;客户端本地解析、中心存储/服务。
- **SC-10** — `code-graph` feature 默认开、加性;`--no-default-features --features
  backend-sqlite` 不含 tree-sitter 仍编译;开启后默认构建 `icm bench` 相对既有基线零回归
  (≤8%)。
- **SC-11** — 可观测:`icm code stats`(符号/边/文件/语言计数 + staleness)+ MCP 暴露;
  部署/使用文档 `docs/code-graph.md`。
- **SC-12** — token 效率佐证:归档一次 before/after(某结构问题:grep/read 爬取 vs 单次
  `icm_code_explore`)显示工具调用数与文件读取显著下降,写入 `docs/code-graph.md`。

### In scope
- SC-1..SC-12;4 语言;本地 + 远程共享;hook 增量同步。

### Out of scope(记为限制)
- 框架感知路由(codegraph 的 17 web 框架 URL→handler)。
- 跨语言桥接(Swift-ObjC / React Native bridge / Expo / Fabric)。
- 原生 OS 文件监视守护进程(FSEvents/inotify)——一期用 PostToolUse hook 增量,不做常驻 watcher。
- 20+ 语言全覆盖(架构可插拔,后续加 grammar)。
- 跨机不同 commit/绝对路径的图合并/冲突消解(一期假设相同相对布局)。

### 用户 / 角色 / 角落场景
- **用户**:在 ICM 集成的 agent(Claude Code 等)+ 开发者 CLI。
- **角落**:超大仓库(解析耗时/内存)、生成文件、符号重名/多定义、动态派发调用无法静态解析
  (尽力而为 + 标注)、增量期间 staleness、远程模式跨机路径差异。

---

## Architecture / Context dependencies

**复用的既有模式(不可偏离):**
- `icm-store` SQLite + FTS5 + `Store` enum + `dispatch!` 宏 —— 新增 `code_graph` 表与
  `CodeGraphStore` 分发臂,和 memory/facts/... 并列。
- `icm-core` trait + 类型分层(`CodeGraphStore` trait + `Symbol`/`Ref`/`CodeFile` 类型)。
- `icm-mcp` 工具分发(新增 `icm_code_explore`)。
- `icm-cli` 命令枚举(新增 `code` 子命令)+ PostToolUse hook(`code_areas` 已存在,复用其
  改动文件流做增量)。
- **F-001 远程栈**:`rpc_dispatch`(服务端)+ `RemoteHttpStore`(客户端)+ `Store::Remote`
  —— 新增 `code.*` RPC 方法,复用同一 JSON-RPC over HTTP 契约(`icm-core::remote_protocol`
  的 `ALL_METHODS` 追加)。

**关键约束(契约):**
- **store 层同步**:tree-sitter 解析是同步 CPU 工作,天然契合;不引入 async 到 core/store。
- **禁 `unwrap`/`expect`** 生产代码;库用 `thiserror`(`IcmError`,可加 `CodeGraph` 变体),
  CLI 用 `anyhow`。
- 新 feature **加性、默认可裁剪**;`--no-default-features` 不含 tree-sitter。
- `clippy -D warnings` 必过。

**跨 crate 影响:**
- icm-core:code-graph 类型 + `CodeGraphStore` trait + language registry(feature `code-graph`);
  `remote_protocol::ALL_METHODS` 追加 `code.*`。
- icm-store:`code_graph` schema + SQLite 实现 + `Store` 分发臂 + `RemoteHttpStore` 的
  `CodeGraphStore` 转发(feature 门)。
- icm-cli:`code` 子命令、`rpc_dispatch` 追加 `code.*` 臂、hook 增量接线、文档。
- icm-mcp:`icm_code_explore` 工具 + 分发。

---

## Environment requirements

- **新增依赖(触发 CI 供应链审计):** `tree-sitter` + `tree-sitter-rust`、
  `tree-sitter-typescript`(含 tsx)、`tree-sitter-python`、`tree-sitter-go`。均为广泛使用的
  官方 grammar crate;grammar 为 C,编入会增大二进制(仅 `code-graph` 开启时)。版本在 plan
  固定并记入审计。
- **新 cargo feature:** `code-graph`(icm-core→icm-store→icm-mcp→icm-cli 透传),**默认开**;
  `code.*` 远程转发随 `remote-store` 组合启用。发布二进制默认含代码图;精简构建可去。
- **运行时:** 索引数据存入 ICM 现有 SQLite(新表);`ICM_DB_BACKEND=remote` 时代码图走中心
  store(客户端本地解析)。
- **CI:** 新增语言解析单测(小样本源码固定装置);远程 round-trip 测试复用 F-001 进程内手法;
  tree-sitter 属重依赖,注意 Windows/macOS/Linux 三平台构建(CI 矩阵已覆盖)。

---

## Definition of Done delta

追加到项目契约 DoD(G4):
1. `cargo fmt/clippy -D warnings/test --workspace` 全绿(默认含 code-graph)。
2. `cargo build --no-default-features --features backend-sqlite` 编译通过(不含 tree-sitter,证明可裁剪)。
3. `cargo test --workspace --features "code-graph,remote-store"` 全绿。
4. 对 ICM 自身仓库执行 `icm code index` 成功;`icm code explore <已知函数>` 返回定义 + callers +
   callees + 逐字源码。
5. 四语言各有解析单测(fixture → 期望符号/边)通过。
6. 增量:编辑一个文件后(经 hook 或 `icm code index --incremental`)该文件符号更新、staleness 清除。
7. 远程 round-trip:客户端本地解析写入 → 另一进程/客户端 `icm code explore` 命中(进程内测试)。
8. 默认构建 `icm bench` 与 `docs/bench-baseline.md` 相差 ≤ 8%(代码图不拖慢既有 memory 路径)。
9. token 效率 before/after 归档于 `docs/code-graph.md`。

---

## Handoff state requirements

- **可观测:** `icm code stats`(符号/引用/文件/语言计数、staleness 文件数)+ MCP 暴露;索引时
  日志打印解析文件数/耗时/跳过项。
- **交接文档:** `docs/code-graph.md` —— 支持语言、index/explore/impact 用法、增量机制、远程共享
  部署与限制(相对路径假设)、token 效率证据、feature 裁剪说明。
- **状态:** `code_graph` 表持久于 SQLite;staleness 标记随 hook 更新;远程模式下中心 store 持有
  共享图。给下一会话:语言 registry 如何扩、跨机路径假设、feature 矩阵。

---

## 7-gate impact map

| Gate | 新约束 |
|---|---|
| **G1 代码** | 4 crate 改动;解析内核 + schema + 查询 + MCP/CLI + 远程转发是主体。 |
| **G2 TDD** | 每语言解析、跨文件解析、增量更新、explore 查询、远程 round-trip 均先写失败测试。 |
| **G3 验证** | 每 SC 有 `cargo test` / `icm code …` / `cargo build --features/--no-default-features` 验证命令。 |
| **G4 DoD** | 追加上节 9 条;默认零回归 + 可裁剪为硬门。 |
| **G5 E2E** | index ICM 仓库 → MCP `icm_code_explore` 单次回答一个真实架构问题(callers+blast-radius),对比 grep 爬取。 |
| **G6 评审** | 关注:同步性、无 unwrap、feature 加性、tree-sitter 依赖审计、远程语义(本地解析/中心存储)、无静默降级。 |
| **G7 交接** | `docs/code-graph.md` + `icm code stats` 观测 + 限制记录到位方可闭合。 |
