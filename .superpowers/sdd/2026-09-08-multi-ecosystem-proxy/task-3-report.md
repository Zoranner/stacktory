# Task 3 报告

## 状态

已完成 Rustup 命名入口迁移。Rustup 现在只注册
`/mirror/rustup/{name}/dist/**`，旧 `/mirror/rustup/dist/**` 路由已删除，
不保留兼容别名或过渡期。

## 变更

- `src/service.rs`
  - Rustup service 改为直接持有 Task 1/2 的 `SourcePool`。
  - `refresh`、manifest 读取和 artifact 读取均接收命名入口。
  - proxy 入口只访问自身；group 只按 `members` 顺序尝试 proxy。
  - manifest 与 `.sha256` 必须来自同一个候选 proxy，并在该候选上完成 URL
    校验和重写。
  - 快照记录实际选中的 proxy；后续 artifact 请求只访问该 proxy，不把 group
    的全部成员混合作为 artifact 来源。
  - 移除对 `legacy_upstream_bases()` 和旧 `UpstreamClient` Rustup service 链路的依赖。
- `src/rustup/manifest.rs`
  - manifest 重写函数接收当前命名入口根地址。
  - 重写后的制品 URL 生成到 `/mirror/rustup/{name}/dist/{path}`。
- `src/http.rs`
  - Rustup 路由改为 `/mirror/rustup/{name}/{*path}`，并严格要求剩余路径以
    `dist/` 开头。
  - 旧路径不再匹配，命名入口错误映射为 404。
- `src/storage.rs`
  - 快照按命名入口隔离，指针记录实际 proxy。
  - artifact cache 按实际 proxy 隔离，避免不同来源共享缓存文件。
- `src/config.rs`
  - 删除 `legacy_upstream_bases()` 及其旧 proxy 遍历辅助逻辑。
- `tests/http_test.rs`
  - 覆盖 group manifest、group 成员回退、proxy 隔离、manifest/artifact 来源
    affinity 和 legacy 路由 404。
- `tests/manifest_test.rs`、`tests/service_test.rs`、`tests/storage_test.rs`、
  `tests/cache_test.rs`
  - 更新命名根、命名快照目录和来源隔离 API 的回归断言。
- `README.md`、`docs/architecture/rustup-mvp.md`
  - 更新命名路由、`RUSTUP_DIST_SERVER` 示例、来源亲和性和旧路由删除说明。

## TDD 与验证

### RED

先加入命名入口测试后运行：

```text
cargo test --test http_test --test manifest_test --test service_test
```

首轮结果：3 项失败、2 项通过。失败为：

- `rustup_group_route_serves_manifest`：实际 404，命名入口尚未接线。
- `rustup_proxy_route_serves_only_its_proxy`：实际 404，命名入口尚未接线。
- `legacy_rustup_route_returns_not_found`：实际 200，旧路由仍注册。

实现过程中还通过回归测试发现并修正了命名路由剩余路径必须剥离
`dist/`、artifact 上游请求必须保留 `dist/` 前缀，以及命名快照过期指针路径
变更等问题。

### GREEN

Rustup focused 回归：

```text
cargo test --test http_test --test manifest_test --test service_test
```

结果：`http_test` 6 passed、`manifest_test` 2 passed、`service_test` 7 passed，
共 15 项通过，0 项失败。

公共来源、路由、缓存和存储回归：

```text
cargo test --test storage_test --test cache_test --test sources_test --test route_test
```

结果：共 16 项通过，0 项失败。

完整检查：

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

结果：

- `cargo fmt --all` 和最终 `cargo fmt --all -- --check`：退出码 0。
- Clippy：退出码 0，无 warning。
- 全量测试：52 项集成测试通过，0 项失败。
- locked release build：退出码 0，成功生成 release 二进制。
- `git diff --check`：退出码 0。

## Concerns

- artifact API 不携带 channel；当同一命名入口的多个当前 channel 都引用相同
  artifact 时，服务按配置 channel 顺序采用第一个可用快照中的 SHA-256 和实际
  proxy。不同 channel 若引用不同 SHA-256，仍返回冲突错误。
- `src/upstream.rs` 及其独立测试仍保留为旧的底层通用实现，但 Rustup service
  已不再使用它；本任务只按简报要求移除 `legacy_upstream_bases()` 和旧 Rustup
  service 路径，未扩大到删除该独立模块。
- 工作树中用户已有的 `src/main.rs`、`docs/plans/2026-08-24-rustup-mirror-mvp.md`
  改动，以及未跟踪的设计、计划和评审文档未纳入本次提交，按要求保留。
