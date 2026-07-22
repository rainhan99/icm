# Features

| ID | Feature | Status | Spec | Plan |
|----|---------|--------|------|------|
| F-001 | 三层共享 Memory 架构(云端 embed + 远程 HTTP store + embedding 缓存可视化) | done | [spec](specs/2026-07-21-three-tier-shared-memory-design.md) | [plan](plans/2026-07-21-three-tier-shared-memory-plan.md) |
| F-002 | 代码图子系统(对标 codegraph,原生 Rust,省 token 查依赖,可跨机共享) | in-progress | [spec](specs/2026-07-22-code-graph-subsystem-design.md) | [plan](plans/2026-07-22-code-graph-subsystem-plan.md) |

## F-001 — Definition of Done 子集

覆盖客户端→服务端→PG 三层:多机共享一个项目 memory、客户端零模型零本地库。
完整 DoD 见 spec 的 `## Definition of Done delta`。核心:默认构建零回归 + 远程 5-trait round-trip 测试 + 缓存可视化端点。
