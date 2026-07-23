# Features

| ID | Feature | Status | Spec | Plan |
|----|---------|--------|------|------|
| F-001 | 三层共享 Memory 架构(云端 embed + 远程 HTTP store + embedding 缓存可视化) | done | [spec](specs/2026-07-21-three-tier-shared-memory-design.md) | [plan](plans/2026-07-21-three-tier-shared-memory-plan.md) |
| F-002 | 代码图子系统(对标 codegraph,原生 Rust,省 token 查依赖,可跨机共享) | done | [spec](specs/2026-07-22-code-graph-subsystem-design.md) | [plan](plans/2026-07-22-code-graph-subsystem-plan.md) |
| F-003 | 远端记忆加固·一期(启发式 supersession + token→tenant 鉴权脚手架,不含数据隔离) | done | [spec](specs/2026-07-22-remote-memory-hardening-design.md) | [plan](plans/2026-07-22-remote-memory-hardening-plan.md) |
| F-003a | 扩 Postgres 后端到全 5 子系统 + PG 测试脚手架(PG 多租户前提) | done | [spec](specs/2026-07-22-postgres-full-subsystem-design.md) | [plan](plans/2026-07-22-postgres-full-subsystem-plan.md) |
| F-003b | PG 行级 tenant + Row-Level Security 数据隔离(依赖 F-003a) | done | [spec](specs/2026-07-23-postgres-tenant-rls-design.md) | [plan](plans/2026-07-23-postgres-tenant-rls-plan.md) |

## F-001 — Definition of Done 子集

覆盖客户端→服务端→PG 三层:多机共享一个项目 memory、客户端零模型零本地库。
完整 DoD 见 spec 的 `## Definition of Done delta`。核心:默认构建零回归 + 远程 5-trait round-trip 测试 + 缓存可视化端点。

## F-003 一期 — Definition of Done(已达成)

- `fmt` / `clippy -D warnings`(默认 + `remote-store,http-api`)/ `test --workspace`(默认 + features)全绿。
- supersession:同 topic 近重复(cosine ≥ `supersede_threshold`,默认 0.90)存新标旧 superseded;
  recall/list/count 默认排除,`--include-superseded` 可见;`≥ 1.0` 关闭 = 今日纯 dedup(向后兼容)。
- token→tenant:`[remote] tokens` 映射;未知/缺失 token→401;无映射回退 F-001 单 token;
  `/whoami` 回显租户(单 token 模式为 `default`);**token 绝不落日志/响应/错误串**。
- 诚实边界:脚手架**只解析身份、不隔离数据**(数据隔离 = F-003b/PG-RLS),代码注释 + `docs/remote-backend.md` §6 醒目警告。
- 旧库自动加 `superseded_at` 列(幂等迁移),既有记忆保持 active。
- 默认 `icm bench --count 5000` 相对 F-002 基线 ≤ 8%(见 `docs/bench-baseline.md`);无新依赖。

## F-003a — Definition of Done(已达成)

- PostgresStore 全子系统对等:FactsStore(6)/MemoirStore(25)/FeedbackStore(6)/
  TranscriptStore(9)共 46 方法从 Unsupported 桩换成真实现;仅 pattern mining
  (明确 out-of-scope)仍 Unsupported。行提取一律 `try_get`+`pg_err`(无 panic 路径),参数化 SQL。
- PG 原生全文检索:`tsvector('simple')` 生成列 + GIN,`plainto_tsquery`/`ts_rank`(memoir/feedback/transcript)。
- 幂等 schema:7 新表 `CREATE ... IF NOT EXISTS`,`init_schema` 跑两次 no-op;所有新表预埋 **nullable `tenant` 列**(F-003a 不填充/不过滤,`memories` 不动=留 F-003b)。
- env-gated 测试:读 `ICM_POSTGRES_URL`,未设自动 skip(默认 `cargo test` 零影响);每子系统 round-trip(live pgvector 8/8 绿)。可选 CI `pgvector/pgvector` service job(非阻塞)。
- 门禁全绿:`fmt` / `clippy -D warnings`(默认 + `--features postgres`)/ `test --workspace`(默认,PG skip)/ `build --features postgres` / 无新依赖。
- 诚实边界:**只功能对等、不做数据隔离**(隔离 = F-003b/PG-RLS);`docs/postgres-backend.md` + 模块注释醒目标注。

## F-003b — Definition of Done(已达成)

- 真隔离:PG 8 表 `ENABLE`+`FORCE ROW LEVEL SECURITY` + policy 按 `current_setting('app.tenant',true)`;
  租户经 HTTP 层解析后由 `Store::set_tenant` 写入连接(参数化 `set_config`,严禁拼串),RLS 在 DB 层过滤读写。
- 自动打标:8 表 tenant 列 `DEFAULT current_setting('app.tenant',true)`;写入自动带租户;memories 列本期补齐。
- 向后兼容:tenant 未设(NULL/'')= 不限制 = 今日行为;SQLite 无 RLS = 单数据集(不变);默认 `cargo test` 全绿(PG 测试 skip)。
- 迁移:已有 NULL-tenant 行回填 `'default'`(FORCE 之前);`init_schema` 幂等(每次重连重跑无错)。
- HTTP 接线:6 处理器(/rpc /recall /store /consolidate /stats /topics)锁定 store 后同锁内 set_tenant 再查(防锁拆分);缺 tenant→500 fail-closed。
- 门禁全绿:`fmt` / `clippy -D warnings`(默认 + postgres + cli http+remote)/ 无新依赖 / 注入 grep(仅 `set_config`)/ live PG 隔离测试 8+3 绿。
- **诚实边界(关键运维要求):** RLS 仅在 **PG + 非超级用户角色**下生效(超级用户绕过 RLS);SQLite 不隔离;直连 CLI/admin(未设租户)不限制。文档 `docs/postgres-backend.md` 醒目标注。
- 三段式多租户闭环:F-003(身份)+ F-003a(全子系统对等)+ F-003b(隔离)= 生产级 PG 多租户。
