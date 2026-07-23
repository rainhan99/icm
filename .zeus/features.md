# Features

| ID | Feature | Status | Spec | Plan |
|----|---------|--------|------|------|
| F-001 | 三层共享 Memory 架构(云端 embed + 远程 HTTP store + embedding 缓存可视化) | done | [spec](specs/2026-07-21-three-tier-shared-memory-design.md) | [plan](plans/2026-07-21-three-tier-shared-memory-plan.md) |
| F-002 | 代码图子系统(对标 codegraph,原生 Rust,省 token 查依赖,可跨机共享) | done | [spec](specs/2026-07-22-code-graph-subsystem-design.md) | [plan](plans/2026-07-22-code-graph-subsystem-plan.md) |
| F-003 | 远端记忆加固·一期(启发式 supersession + token→tenant 鉴权脚手架,不含数据隔离) | done | [spec](specs/2026-07-22-remote-memory-hardening-design.md) | [plan](plans/2026-07-22-remote-memory-hardening-plan.md) |
| F-003a | 扩 Postgres 后端到全 5 子系统 + PG 测试脚手架(PG 多租户前提) | done | [spec](specs/2026-07-22-postgres-full-subsystem-design.md) | [plan](plans/2026-07-22-postgres-full-subsystem-plan.md) |
| F-003b | PG 行级 tenant + Row-Level Security 数据隔离(依赖 F-003a) | planned | — | — |

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
