# Spec — 远端记忆加固(一期:supersession + token→tenant 鉴权脚手架)(F-003)

- **Feature:** F-003(一期;PG 多租户隔离拆为后续 F-003a/F-003b)
- **Date:** 2026-07-22(重定向后重写)
- **Status:** Draft(待用户批准)
- **Brainstorm mode:** [2] 两方案对比 → 重定向后收窄
- **Contract anchored:** `CLAUDE.md` + `AGENTS.md`
- **参考(仅设计,不复制源码):** github.com/supermemoryai/supermemory

---

## Goal / Scope

### 背景(重定向)
用户生产目标 = **200 人团队 + Postgres 多租户**(见记忆 `deployment-context`)。真正的
数据隔离应基于 **PG 行级 tenant + Row-Level Security**,并以"补全 PG 后端到全 5 子系统"
为前提 —— 那是一个多会话大工程,拆为后续:
- **F-003a**(planned):扩 postgres 后端到全 5 子系统 + PG 测试脚手架。
- **F-003b**(planned):PG 行级 tenant + RLS 数据隔离(依赖 a)。

**本期(F-003)只做 backend-无关、现在可测、且能独立交付价值的两块:**
1. **③ 启发式近重复 supersession**:提升记忆质量,SQLite 上即可验证。
2. **token→tenant 鉴权脚手架**:硬化 F-001 单一共享 token —— 服务端 `token→tenant`
   映射、未知 token 拒绝、解析出租户身份并入请求上下文/日志,为 F-003b 的 RLS 隔离
   铺好插座。

### 明确的非目标(安全诚实底线)
**本期鉴权脚手架只解析/记录租户身份,不隔离已存数据** —— 多租户**数据隔离**属 F-003b
(PG RLS),本期不提供。文档必须醒目标注,避免误判"已隔离"。

### Scope Checklist
- **SC-1** — memory `superseded` 状态:`memories` 加布尔/状态列(幂等迁移,默认未废弃);
  recall/list 默认排除 superseded;提供 include 开关。`tenant=None` 现有行为不变。
- **SC-2** — 启发式 supersession:`store` 时同 topic 内与新记忆相似度 > `supersede_threshold`
  (默认 0.90)的旧记忆被标 superseded(而非并存);阈值可配;扩展现有 `find_similar_memory`。
- **SC-3** — supersession 可配/可关:`[memory] supersede_threshold`;阈值 ≥ 1.0 或缺省关闭时
  行为等于今日(纯 dedup),向后兼容。
- **SC-4** — token→tenant 配置:服务端读 `[remote] tokens`(`token = tenant` 映射)或独立
  `tokens.toml`;加载为 `token→tenant` 查找表。
- **SC-5** — 鉴权解析:`auth_middleware` 用映射把 Bearer token 解析为租户;**未知 token → 401**;
  无 tokens-map 配置时回退 F-001 单 token 行为(加性、向后兼容)。
- **SC-6** — 租户身份入上下文 + 观测:解析出的租户放入请求上下文(供 F-003b 用)并记入日志;
  **绝不打印 token 明文**;新增 `/whoami`(或 `/health` 扩展)回显当前租户(不回显 token)。
- **SC-7** — 文档 + 诚实边界:`docs/remote-backend.md` 增"一期鉴权脚手架"节:tokens 配置、
  未知 token 拒绝、**"本期仅身份解析,尚未做数据隔离,隔离见 F-003b/PG-RLS"** 的醒目警告 +
  supersession 语义/阈值。
- **SC-8** — 零回归 + 无新 feature:随 `remote-store`/`http-api`;默认/本地 `tenant=None`、
  无 tokens-map、supersession 关闭时,行为与今日逐字节一致。

### In scope
- SC-1..SC-8:supersession(记忆质量)+ token→tenant 身份解析与鉴权硬化(不含数据隔离)。

### Out of scope(明确延后/记为限制)
- **多租户数据隔离**(行级 tenant 过滤 / RLS)→ F-003b。
- **扩 PG 后端到全子系统** → F-003a。
- 真语义对立检测(需 LLM);细粒度 RBAC/配额;TLS。
- 一 token 多租户(本期一 token 一租户)。

### 角落场景
- supersede 阈值边界(0.90 附近)、被 supersede 记忆的 recall 可见性、consolidation 与
  supersession 交互;未知 token、无 tokens-map 回退、token 前后空白、tenant 名特殊字符。

---

## Architecture / Context dependencies
- **supersession**:扩展 `find_similar_memory`(0.85 dedup 阈值)+ `store` 路径;memory 加
  superseded 列(schema.rs 幂等迁移,类比现有列迁移);recall/list 过滤。backend-无关(经
  MemoryStore;SQLite 现在可测,PG 迁移时同列)。
- **鉴权脚手架**:复用 F-001 `http_api::auth_middleware`(现为单 token 比对)——改为查
  `token→tenant` 表;`AppState` 增 tokens 映射 + 每请求 tenant 上下文;config 读取。
- 与 `Scope`(User/Project/Org)正交;与 F-002 代码图无关。
- **约束**:store 同步;禁 unwrap/expect;加性、默认可裁;clippy -D warnings;默认零回归。

**跨 crate 影响:** icm-core(supersession 逻辑 + memory 列/状态);icm-store(schema 迁移 +
查询过滤 superseded);icm-cli(auth_middleware token→tenant + tokens 配置 + /whoami + 文档)。

---

## Environment requirements
- **无新增第三方依赖**:`toml`(配置)已在树。
- **配置**:`[remote] tokens`(映射)、`[memory] supersede_threshold`(默认 0.90;≥1.0 关闭)。
- **无新 cargo feature**:随 `remote-store`/`http-api`;supersession 在 memory store(默认可用,
  阈值关则等同今日)。
- **CI**:supersession 单测(SQLite in-memory)、token→tenant 鉴权测试(进程内 tower oneshot)、
  迁移测试(旧库加 superseded 列);`cargo audit`(无新依赖)。

---

## Definition of Done delta
1. `cargo fmt / clippy -D warnings / test --workspace` 全绿(默认 + `remote-store,http-api`)。
2. supersession:存 "user lives in NYC" 再存 "user moved to SF"(同 topic、高相似)→ 旧记忆
   标 superseded;`recall` 默认返回 SF、不返回 NYC;`--include-superseded` 可见旧的。
3. supersede_threshold ≥ 1.0(或关闭)时,store 行为等于今日纯 dedup(向后兼容测试)。
4. token→tenant:配置两 token→两租户;未知 token → 401;已知 token 解析出对应租户(日志/whoami
   可见,token 不出现在日志)。
5. 无 tokens-map 时,远程 store 鉴权行为与 F-001 单 token 一致(现有远程测试仍绿)。
6. 迁移:F-001 旧库打开后自动加 superseded 列,既有记忆视为未废弃,可正常读写。
7. 默认 `icm bench` 与基线 ≤ 8%(supersession 关闭/default 路径零回归)。
8. 文档含"尚未数据隔离"醒目警告 + F-003a/b 路线。

---

## Handoff state requirements
- **可观测**:`/whoami` 回显当前租户;日志含解析租户(无 token 明文);supersession 计数可经
  `icm stats` 或日志观察。
- **交接文档**:`docs/remote-backend.md` 一期鉴权脚手架节 + supersession 节 + 非隔离警告 + 后续
  F-003a(扩 PG)/F-003b(RLS 隔离)路线。
- **不变量/状态**:"鉴权脚手架≠数据隔离" 写入文档与代码注释;supersession 阈值语义;
  `tenant=None`/无 tokens-map = 向后兼容。给下一会话:F-003a/b 是 PG 专项、需活 PG + pgvector。

---

## 7-gate impact map
| Gate | 新约束 |
|---|---|
| **G1 代码** | icm-core supersession + icm-store 迁移/过滤 + icm-cli auth token→tenant/whoami。 |
| **G2 TDD** | supersession、向后兼容(阈值关)、token→tenant 401/解析、迁移均先写失败测试。 |
| **G3 验证** | 每 SC 有 `cargo test` / 进程内 oneshot / 迁移测试命令。 |
| **G4 DoD** | 追加上节 8 条;向后兼容 + 零回归 + "非隔离"文档为硬门。 |
| **G5 E2E** | 进程内 server:两 token→两租户解析 + 未知 401;store 触发 supersession → recall 反映。 |
| **G6 评审** | token 不落日志;supersession 不误废非近重复;同步/无 unwrap;**文档诚实标注非隔离**。 |
| **G7 交接** | 文档 + /whoami + 非隔离警告 + F-003a/b 路线到位。 |
