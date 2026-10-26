# Stacktory 后端结构迁移实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `src/` 迁移到 `docs/specs/2026-10-26-backend-structure-design.md` 的目标结构，行为不变。

**Architecture:** 第一层按运行时职责划分，业务只在 `ecosystems/` 第二层；大文件按协议对象和生命周期拆分；三处反向依赖通过 `storage::access`、`Maintainer` 注入和 `SiteContext` 倒置。

**Tech Stack:** Rust 2024、axum、tokio、toml/serde、cargo test。

**Spec:** `docs/specs/2026-10-26-backend-structure-design.md`

## Global Constraints

- 不改变路由、配置 schema、数据目录、审计与日志格式、错误分类。
- 不提交、不推送；提交需单独授权。
- 现有未提交的业务修复随文件迁移保留，不重排、不回退。
- 每批结束必须能编译并通过该批定向测试；不得留下旧路径或兼容转发。
- 依赖方向遵守设计稿的允许依赖表；门禁用 `rg` 检查。

---

### Task 1: 契约与底座

**Files:**
- Create: `src/config/mod.rs`、`src/config/model.rs`、`src/config/parse.rs`、`src/config/validate.rs`
- Move: `src/layout.rs` → `src/storage/layout.rs`；`src/ops/audit.rs` → `src/audit/mod.rs`；`src/ops/logging.rs` → `src/logging/mod.rs`；`src/ops/rotation.rs` → `src/logging/rotation.rs`；`src/ops/retention.rs` → `src/retention/mod.rs`
- Create: `src/storage/access.rs`（`note_access` 从 retention 迁出）
- Modify: `src/lib.rs`、全部引用方
- Test: `tests/config_test.rs`、`tests/retention_test.rs`、`tests/cache_test.rs`、`tests/storage_test.rs`

- [x] 移动文件并拆分 `config.rs`，保持公共类型与 `Config::from_toml` 签名不变。
- [x] 把 `note_access` 移到 `storage/access.rs`，适配器只依赖 `storage`。
- [x] 在 `retention` 定义 `Maintainer` 回调类型，`startup` 注册 Rustup 快照维护入口。
- [x] 更新 `crate::` 与 `stacktory::` 引用，删除 `src/ops`。
- [x] 验证：`cargo fmt --all`、`cargo check --all-targets`、`cargo test --test config_test --test retention_test --test cache_test --test storage_test`。

### Task 2: 上游访问

**Files:**
- Move: `src/sources/{mod,pool,fallback,registry}.rs` → `src/upstream/`
- Move: `src/sources/fetcher.rs` → `src/upstream/fetch/mod.rs`；`src/sources/policy.rs` → `src/upstream/fetch/policy.rs`
- Create: `src/upstream/budget.rs`、`src/upstream/group.rs`（从 `pool.rs` 拆出预算与 group 循环）
- Test: `tests/sources_test.rs`、`tests/route_test.rs`、各生态 `*_group_test.rs`

- [x] 重命名目录并更新 `stacktory::sources` 与 `crate::sources` 引用。
- [x] 拆分 `pool.rs` 的制品预算与 group 成员循环，保持公开类型再导出。
- [x] 验证：`cargo check --all-targets`、`cargo test --test sources_test --test route_test`。

### Task 3: 适配器目录与缓存拆分

**Files:**
- Modify: `src/ecosystems/rustup/{mod,cache}.rs` → `cache/{mod,snapshots,artifacts}.rs`、`maintain.rs`；`path.rs` → `dist.rs`；`service.rs` → `service/mod.rs`
- Modify: `src/ecosystems/npm/storage.rs` → `cache/{mod,metadata,artifacts,integrity}.rs`
- Test: `tests/cache_test.rs`、`tests/storage_test.rs`、`tests/npm_cache_test.rs`、`tests/service_test.rs`

- [x] 拆 Rustup 快照与制品缓存，保留 `ecosystems::rustup` 的公开再导出。
- [x] 拆 npm 元数据、制品与完整性校验缓存。
- [x] 验证：`cargo test --test cache_test --test storage_test --test npm_cache_test --test service_test`。

### Task 4: 大文件拆分

**Files:**
- Modify: `src/ecosystems/docker/` → `mod.rs`、`path.rs`、`manifest.rs`、`blob.rs`、`query.rs`、`cache.rs`
- Modify: `src/ecosystems/npm/service.rs` → `service/{mod,search,metadata,tarball}.rs`
- Modify: `src/ecosystems/nuget/{service_index,registration,package}.rs` → 各自 `{mod,cache,protocol}.rs`
- Modify: `src/ecosystems/cargo/service.rs` → `service/{mod,index,artifact}.rs`
- Modify: `src/ecosystems/vscode/service.rs` → `mod.rs`、`query.rs`、`assets.rs`、`cache.rs`
- Test: `tests/docker_*`、`tests/npm_*`、`tests/nuget_*`、`tests/cargo_*`、`tests/vscode_protocol_test.rs`

- [x] 每拆一个生态立即编译并跑该生态全部测试，失败不进入下一个生态。
- [x] 只做代码移动、可见性调整和 `mod`/`use` 更新，不改协议行为。
- [x] 验证：对应测试文件逐个通过。

### Task 5: 入口装配

**Files:**
- Move: `src/app/{mod,serve,registry}.rs` → `src/startup/`
- Create: `src/commands/{mod,prune_legacy_layout,invalidate_nuget_metadata}.rs`
- Move: `src/http/guide.rs` + `src/guide/*` → `src/http/guide/`；`src/http/rustup.rs` → `src/http/rustup_dist.rs`
- Modify: `src/http/{mod,site,mirror}.rs`、`src/main.rs`、`src/lib.rs`
- Test: `tests/http_test.rs`、`tests/docker_listener_test.rs`

- [x] 引入 `SiteContext`，首页与指南路由持有站点上下文，镜像路由持有注册表，Rustup 入口持有服务。
- [x] `startup/registry.rs` 用通用注册函数收口 SourcePool/Adapter 构造；删除 `router_with_rustup_and_cargo`。
- [x] 删除旧 `src/app`、`src/guide`。
- [x] 验证：`cargo test --test http_test --test docker_listener_test`。

### Task 6: 收口与验证

**Files:**
- Modify: `docs/architecture/multi-ecosystem-readonly-proxy.md`
- Verify: 全仓库

- [x] 递归检查旧路径不存在：`src/app`、`src/guide`、`src/sources`、`src/ops`、`src/service.rs`、`src/storage.rs`、`src/layout.rs`、`src/http.rs`。
- [x] 依赖门禁：`rg "crate::(retention|ecosystems)" src/ecosystems src/retention` 只允许 `ecosystems` 侧为零，`rg "crate::ecosystems" src/retention` 为空。
- [x] `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-targets --all-features`。
- [x] 更新架构文档中的 `src/layout.rs` 路径引用。
- [x] 汇总未验证边界：真实上游、真实客户端、部署环境。

## 执行边界

迁移已完成并通过全量测试；旧模块路径和兼容转发已删除。提交、推送和部署仍待用户
单独授权。真实上游、真实客户端和部署环境未在本次验证范围内。
