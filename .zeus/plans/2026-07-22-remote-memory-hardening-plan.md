# Plan — 远端记忆加固·一期 (F-003)

- **Spec:** [.zeus/specs/2026-07-22-remote-memory-hardening-design.md](../specs/2026-07-22-remote-memory-hardening-design.md)
- **Feature:** F-003(一期;PG 隔离拆为 F-003a/F-003b)
- **Date:** 2026-07-22
- **Status:** Draft — 待用户签名批准

---

## 1. Header
**Goal:** 一期两块 backend-无关、可测:③ 启发式 supersession(记忆质量)+ token→tenant 鉴权脚手架(硬化 F-001 单 token,只解析身份、**不隔离数据**)。

**Architecture(已定):**
- supersession:`Memory.superseded_at: Option<DateTime>`(`#[serde(default)]`);标记 = `get→set→update()`(**复用现有 update,不新增 trait 方法**);SqliteStore 迁移加列、update 写列、记忆读查询过滤 `superseded_at IS NULL`;icm-core `supersede_similar` helper(复用 `find_similar_memory`);阈值 `[memory] supersede_threshold`(默认 0.90,≥1.0 关闭=今日行为)。postgres/opensearch 该字段不持久 → None → 不 supersede(一期 SQLite 目标)。
- 鉴权脚手架:`[remote] tokens`(token→tenant)→ `AppState` tokens map;`auth_middleware` 解析 Bearer→租户,未知→401;租户入请求上下文 + 日志(token 不落);`/whoami` 回显租户;无 tokens-map 回退 F-001 单 token。

**Tech Stack:** Rust · SQLite/SQL · HTTP API。**F-NNN:** F-003(一期)。

---

## 2. File Map
### 修改
| 文件 | 改动 |
|---|---|
| `crates/icm-core/src/memory.rs` | `Memory.superseded_at: Option<DateTime<Utc>>`(`serde(default)`)+ `is_active()` |
| `crates/icm-core/src/store.rs` | `supersede_similar(store, &new, threshold) -> IcmResult<Option<String>>` helper(复用 `find_similar_memory` + `update` 标记旧) |
| `crates/icm-store/src/schema.rs` | `memories` 幂等迁移 `ADD COLUMN superseded_at TEXT` |
| `crates/icm-store/src/store.rs` | 行映射读 `superseded_at`;`store_inner`/`update` 写列;记忆读查询(search_hybrid/fts/keywords/list_all/get_by_topic/topic_health/count)加 `superseded_at IS NULL` |
| `crates/icm-cli/src/config.rs` | `[remote] tokens`(map)+ `[memory] supersede_threshold`(默认 0.90) |
| `crates/icm-cli/src/http_api.rs` | `AppState` tokens map + tenant 上下文;`auth_middleware` token→tenant;`/whoami` |
| `crates/icm-cli/src/main.rs` | `cmd_serve` 载 tokens 配置;`cmd_store` 接 `supersede_similar`;`recall` 增 `--include-superseded` |
| `crates/icm-mcp/src/tools.rs` | `tool_store` 接 supersession;recall 工具 include-superseded 参数 |
| `docs/remote-backend.md` | 一期鉴权脚手架节(+**非隔离醒目警告**)+ supersession 节 + F-003a/b 路线 |
| `config/default.toml` | `supersede_threshold` + `[remote] tokens` 注释 |

### 新建
| 文件 | 职责 |
|---|---|
| (无新文件) | 全部为对现有文件的加性改动 |

---

## 3. Architect Risk Analysis(经用户确认)
**Rust 架构师** — *Restate:* Memory 加 superseded_at + helper,复用 update。 *Risk:* 记忆读查询多处需加过滤谓词,漏一处则 superseded 记忆仍出现在 recall(非安全泄漏,是质量 bug);逐查询核对(G6)。零回归靠"无 supersede 时谓词匹配全部 + 迁移默认 NULL"。 *已答:* 不新增 trait 方法(复用 update),postgres/opensearch 不持久该字段=不 supersede(一期)。

**SQLite/SQL 架构师** — *Restate:* memories 加列 + 读过滤。 *Risk:* ADD COLUMN 幂等(O(1) 元数据);FTS/vec 是 external-content over memories,过滤落 memories join;`is_active` = `superseded_at IS NULL`;consolidation 与 supersession 不冲突(consolidate 另路径)。

**HTTP/鉴权架构师** — *Restate:* token→tenant 查表 + /whoami。 *Risk:* **token 绝不落日志**(只记 tenant);未知 token 401;无 tokens-map 回退单 token;`/health` 仍免鉴权,`/whoami` 需鉴权;tenant 上下文经请求扩展传递,不污染 F-001 无租户路径。 *诚实边界:* **不隔离数据**——文档 + 代码注释醒目标注。

**用户已确认:** supersession 复用 update、SQLite 目标;鉴权仅身份解析非隔离。

---

## 4. Tasks
> TDD:①失败测试(全码)②确认失败③最小实现④确认通过⑤提交。PATH 前缀
> `$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin`。

### Stage A — supersession(③)

#### T1 — `Memory.superseded_at` 字段
1. **失败测试**(memory.rs):`Memory::new(...)` 的 `superseded_at` 为 None 且 `is_active()` 为 true;旧 JSON(无该字段)反序列化 → None(serde default)。
2. **确认失败:** `cargo test -p icm-core memory::tests::superseded_default` → 无字段/方法。
3. **最小实现:** 加 `#[serde(default)] pub superseded_at: Option<DateTime<Utc>>` + `pub fn is_active(&self)->bool { self.superseded_at.is_none() }`;`Memory::new` 初始化 None。
4. **确认通过 / 提交:** `feat(core): add Memory.superseded_at + is_active`。

#### T2 — memories 迁移加列 + 行映射 + update 写列
1. **失败测试**(icm-store):`SqliteStore::in_memory_with_dims(384)`;store 一条,update 设 `superseded_at=Some(now)`,再 `get` → superseded_at 有值。
2. **确认失败:** `cargo test -p icm-store memory_superseded_persist`。
3. **最小实现:** schema.rs `ADD COLUMN superseded_at TEXT`(幂等 guard);row_to_memory 读列;`store_inner`/`update` 写列(rfc3339/NULL)。
4. **确认通过 / 提交:** `feat(store): persist memories.superseded_at (migration + read/write)`。

#### T3 — 读查询默认排除 superseded
1. **失败测试:** store 两条同 topic;把其一 update 为 superseded;`search_fts`/`list_all`/`get_by_topic` 均只返回 active 的那条;`count` 计 1。
2. **确认失败:** `cargo test -p icm-store reads_exclude_superseded`。
3. **最小实现:** 记忆读查询(search_hybrid/search_fts/search_by_keywords/list_all/get_by_topic/count/topic_health)加 `AND superseded_at IS NULL`;`get(id)` 仍按 id 返回(直取不过滤)。
4. **确认通过 / 提交:** `feat(store): exclude superseded memories from recall/list by default`。

#### T4 — `supersede_similar` helper + 阈值
1. **失败测试**(icm-core,用 in-memory SqliteStore):存 "user lives in NYC"(topic=profile);`supersede_similar` 处理新 "user moved to SF"(同 topic、高相似)→ 返回被 supersede 的旧 id,旧记忆 `is_active()`=false;阈值设 1.0 时返回 None(不 supersede)。
2. **确认失败:** `cargo test -p icm-core supersede_similar_marks_old`(注:相似度用真实 embedder 不可控 → 测试用可控 stub embedder 或直接比较 topic + 注入相似;helper 以 `find_similar_memory` 的 score 为准,测试构造 score 可控的场景)。
3. **最小实现:** `supersede_similar(store, new, threshold)`:`find_similar_memory(store, embed_text, embedding, topic, threshold)` 命中 → `get→set superseded_at→update`,返回旧 id;否则 None。阈值 ≥1.0 直接返回 None。
4. **确认通过 / 提交:** `feat(core): supersede_similar helper (heuristic near-dup temporal replacement)`。

#### T5 — 接线 CLI/MCP store + `--include-superseded`
1. **失败测试:** `cmd_store` 路径(单测/集成)存近重复后旧记忆被标 superseded;recall 默认不含、`--include-superseded` 含。MCP `tool_store` 同。
2. **确认失败:** `cargo test -p icm-cli supersession_wired`。
3. **最小实现:** `cmd_store`/`tool_store` 在存新记忆后调 `supersede_similar`(读 `supersede_threshold`);`recall` CLI/MCP 增 include-superseded(经一个 list-all-including 查询或参数)。
4. **确认通过 / 提交:** `feat(cli,mcp): wire supersession into store + --include-superseded recall`。

### Stage B — 鉴权脚手架

#### T6 — `[remote] tokens` 配置
1. **失败测试**(config.rs):解析 `[remote] tokens = { "tokA" = "tenantA" }` → map 含 tokA→tenantA。
2. **确认失败:** `cargo test -p icm-cli remote_tokens_parse`。
3. **最小实现:** `RemoteConfig { tokens: HashMap<String,String> }`(或 `[remote]` 段)+ 默认空。
4. **确认通过 / 提交:** `feat(cli): [remote] tokens config (token→tenant map)`。

#### T7 — auth_middleware token→tenant + 401
1. **失败测试**(http_api oneshot):AppState 带 tokens map(tokA→tA);`Bearer tokA` 请求 `/whoami` → 200 且体含 "tenantA";`Bearer bad` → 401;无 tokens-map + 单 token 模式 → 沿用 F-001。
2. **确认失败:** `cargo test -p icm-cli --features "http-api,remote-store" auth_token_to_tenant`。
3. **最小实现:** `AppState` 增 `tokens: Option<HashMap<String,String>>`;`auth_middleware` 若 tokens map 存在 → 解析 Bearer→tenant(未知 401),把 tenant 存入 request extensions;否则回退现有单 token 比对;**日志只记 tenant**。
4. **确认通过 / 提交:** `feat(cli): token→tenant auth resolution (unknown→401), identity only`。

#### T8 — `/whoami` 端点
1. **失败测试:** `/whoami` 带有效 token 返回 `{"tenant": "..."}`(单 token 模式返回 "default");不回显 token。
2. **确认失败:** `cargo test -p icm-cli --features "http-api,remote-store" whoami_endpoint`。
3. **最小实现:** `GET /whoami` handler 读 request extensions 的 tenant(或 "default"),返回 JSON;经鉴权中间件。
4. **确认通过 / 提交:** `feat(cli): /whoami echoes resolved tenant (never token)`。

### Stage C — 文档 + 门禁

#### T9 — 文档 + config 注释
1. **验证(可命令):** `grep -qi "尚未.*隔离\|not.*isolat" docs/remote-backend.md`(非隔离警告存在)+ `grep -q supersede_threshold config/default.toml`。
2. **实现:** `docs/remote-backend.md` 增鉴权脚手架节(tokens 配置、未知拒绝、**"仅身份解析,尚未数据隔离,隔离见 F-003a/b"** 醒目警告)+ supersession 节;`config/default.toml` 注释。
3. **提交:** `docs: F-003 phase-1 auth scaffold (non-isolation warning) + supersession`。

#### T10 — 零回归 + 全门禁
1. **验证:** `cargo fmt --check`;`clippy --workspace --all-targets -- -D warnings`(默认 + `remote-store,http-api`);`cargo test --workspace`(默认 + features)全绿;`./target/release/icm bench --count 5000` 与 `docs/bench-baseline.md` ≤8%(supersede 关/default 路径)。
2. **实现:** 修复门禁问题;bench 归档确认。
3. **提交:** `chore(f-003): verify gates + zero-regression`。

---

## 5. Test Plan
**Unit:** memory superseded_at/is_active(T1)、迁移+读写(T2)、读排除(T3)、supersede_similar(T4)、tokens 解析(T6)。
**Integration:** CLI/MCP store 触发 supersession + include-superseded(T5)、auth token→tenant/401(T7)、/whoami(T8)。
**E2E:** 进程内 server:两 token→两租户经 /whoami 解析 + 未知 401;store NYC→SF 使 NYC superseded、recall 返回 SF。
**Regression:** `cargo test --workspace`(默认)全绿(无 supersede、无 tokens-map 时行为不变);`supersede_threshold≥1.0` 时 store=今日 dedup;`icm bench` ≤8%。

---

## 6. Security Review
**静态:** `cargo audit`(无新依赖)、`clippy -D warnings`。
**威胁模型(新面):**
- **凭据泄漏**:token→tenant 映射中的 token 绝不进日志/错误串/`/whoami`;只记 tenant。config 文件权限由运维负责(文档提示)。
- **鉴权绕过**:未知/缺失 token → 401(除 `/health`);无 tokens-map 回退单 token 不得意外放开。
- **诚实边界(关键)**:脚手架**不隔离数据**——严禁文档/命名暗示已隔离,防误部署;代码注释 + 文档醒目警告。
- **supersession 误废**:仅同 topic 且相似度 > 阈值才 supersede;阈值默认保守(0.90);不跨 topic;get(id) 仍可直取(可审计)。
**具体检查:** 输入校验(tokens 配置解析)、secrets(token 不落日志)、auth(401 + 回退)、无注入(参数化 SQL)、无 SSRF/路径。

---

## 7. Logic Review Checkpoints
- **CP-A(Stage A 后):** 每个记忆读查询是否都加 `superseded_at IS NULL`?(逐查询核对,漏=质量 bug)零回归:无 supersede 行为等同今日?supersede 仅同 topic + 阈值?
- **CP-B(Stage B 后):** token 是否任何路径都不落日志?未知 token 401?无 tokens-map 回退正确?tenant 上下文不污染无租户路径?`/whoami` 不回显 token?
- **CP-C(文档):** 非隔离警告是否醒目、不误导?

---

## 8. G4 contract delta
1. `fmt/clippy -D warnings/test --workspace`(默认 + remote-store,http-api)全绿。
2. supersession:NYC→SF 使旧标 superseded,recall 默认返回 SF,`--include-superseded` 见旧。
3. `supersede_threshold≥1.0`/缺省关 → store 行为等于今日(向后兼容)。
4. token→tenant:未知 401;有效解析出租户(/whoami、日志),token 不落日志。
5. 无 tokens-map → 鉴权同 F-001 单 token(现有远程测试仍绿)。
6. 旧库自动加 superseded_at 列,既有记忆 active,读写正常。
7. 默认 `icm bench` ≤8%。
8. 文档含非隔离醒目警告 + F-003a/b 路线。

---

## 9. Logic Completeness Manifest
**Every requirement in the linked spec MUST be implemented in full. Authorized simplifications: (none).**

> 说明:spec `## Out of scope`(PG 数据隔离/RLS = F-003b、扩 PG 后端 = F-003a、LLM 矛盾检测、RBAC/配额、TLS)是**已批准的范围边界/拆分**,非对一期 SC 的简化。postgres/opensearch 不 supersede 是"该字段不持久=None"的自然结果(一期 SQLite 目标),spec 已述,非简化。

### Spec Coverage Matrix
| SC-ID | Capability | 实现任务 | 验证命令 |
|---|---|---|---|
| SC-1 | memories superseded 状态 + 迁移 | T1, T2 | `cargo test -p icm-store memory_superseded_persist` |
| SC-2 | 启发式 supersession(store 时标旧) | T4, T5 | `cargo test -p icm-core supersede_similar_marks_old` |
| SC-3 | 阈值可配/关=向后兼容 | T4, T5 | `cargo test -p icm-core supersede_similar_marks_old`(阈值1.0 分支) |
| SC-4 | token→tenant 配置 | T6 | `cargo test -p icm-cli remote_tokens_parse` |
| SC-5 | 鉴权解析 + 未知 401 + 回退 | T7 | `cargo test -p icm-cli --features "http-api,remote-store" auth_token_to_tenant` |
| SC-6 | 租户入上下文 + /whoami + 不落 token | T7, T8 | `cargo test -p icm-cli --features "http-api,remote-store" whoami_endpoint` |
| SC-7 | 文档 + 诚实非隔离边界 | T9 | `grep -qi "not.*isolat\|尚未.*隔离" docs/remote-backend.md` |
| SC-8 | 零回归 + 无新 feature | T3, T10 | `cargo test --workspace && ./target/release/icm bench --count 5000` |

**手工覆盖检查声明(降级):** `scripts/check-spec-coverage.sh` 不存在;人工核对 SC-1..SC-8 均映射 ≥1 任务,无孤儿。

---

## 10. File Size Constraints
阈值取默认表;全部为对现有文件的加性改动,无新建。
| 文件 | 类型 | 预估增量 | 判定 |
|---|---|---|---|
| `memory.rs`(改) | 类型 | +6 | OK |
| `store.rs`(icm-core,改) | helper | +30 | OK |
| `schema.rs`(改) | schema(relaxed) | +8 | OK |
| `store.rs`(icm-store,改) | **既有 7433,预存偏大** | +40(读过滤谓词分散) | 既有大文件,加性微改;不新增结构性膨胀 |
| `config.rs`(改) | config(relaxed) | +15 | OK |
| `http_api.rs`(改) | controller | +70 → ~1050 | (relaxed, controller);auth/whoami 逻辑内聚,逼近则抽 `auth.rs` |
| `main.rs`(改) | **既有 ~10.9k,预存 OVER** | +30 | 既有超标,外科式加性;supersession 逻辑在 icm-core helper,main 仅接线 |
| `tools.rs`(改) | MCP | +25 | OK |
| `docs/remote-backend.md`(改) | 文档(relaxed) | +60 | OK |

无新建文件越界;大文件均为既有、仅加性微改。

---

**User-approved:** 2026-07-22 by rainhan@coupert.com
