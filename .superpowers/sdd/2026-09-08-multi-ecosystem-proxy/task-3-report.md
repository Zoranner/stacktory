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

## 原始 Task 3 Concerns

- artifact API 不携带 channel 的问题已由本轮“审查修复追加”解决；原始交付时
  该问题仍存在。
- `src/upstream.rs` 及其独立测试仍保留为旧的底层通用实现，但 Rustup service
  已不再使用它；本任务只按简报要求移除 `legacy_upstream_bases()` 和旧 Rustup
  service 路径，未扩大到删除该独立模块。

## 审查修复追加

### Findings

- Important 1：artifact 请求原先扫描 `self.channels` 查找首个 hash，无法表达
  manifest 所属 channel 和实际 proxy。现改为 manifest URL 使用
  `/mirror/rustup/{name}/dist/{channel}/{path}`，artifact service 显式接收
  `entry_name`、`channel` 和 `DistPath`，只读取对应快照记录的 hash/source。
- Important 2：artifact checksum 查询前新增 source entry 校验。未知 source 返回
  404，已配置但没有该 channel 快照仍返回 503。
- Important 3：补充成功 artifact、正确 proxy 命中和 cache 隔离测试；cache 路径
  现在按 `entry/channel/proxy/path` 隔离。

### TDD 与验证

先加入审查回归后运行：

```text
cargo test --test http_test --test manifest_test --test service_test
```

结果为预期红灯：编译器报告 `rewrite_manifest` 尚未接受 channel 参数，
`artifact_for_request` 尚未接受 channel 参数；这证明新测试确实约束了旧实现
无法表达的来源绑定模型。

补齐测试支撑后再次运行，仍因生产接口未迁移失败；随后实现 channel 绑定、source
entry 前置校验和三维 cache 隔离。

修复后的 focused 回归：

```text
cargo test --test http_test --test manifest_test --test service_test
```

结果：`http_test` 7 passed、`manifest_test` 2 passed、`service_test` 8 passed，
共 17 项通过，0 项失败。新增多 channel 成功路径确认 stable 从第一个 proxy
获取并缓存，beta 从第二个 proxy 获取并缓存，两个 cache 文件路径彼此独立；
新增 HTTP 回归确认未知 source 为 404、已配置无 snapshot 为 503。

本轮最终检查：

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

结果：

- `cargo fmt --all`：通过。
- `cargo clippy --all-targets --all-features -- -D warnings`：通过，无 warning。
- `cargo test --all-targets --all-features`：54 项集成测试通过，0 项失败。
- `cargo build --release --locked`：通过，生成 release 二进制。

本轮修复不引入数据库，不改变 SourcePool 或配置边界，不恢复旧路由。
- 工作树中用户已有的 `src/main.rs`、`docs/plans/2026-08-24-rustup-mirror-mvp.md`
  改动，以及未跟踪的设计、计划和评审文档未纳入本次提交，按要求保留。
