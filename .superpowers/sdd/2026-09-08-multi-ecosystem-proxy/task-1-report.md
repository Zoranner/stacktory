# Task 1 报告

## 状态

已完成配置模型与 source graph。实现保留当前工作树中的
`listen_address`、`listen_port` 和 `root_url` 命名变更，并将 Rustup 配置迁移为
`rustup.sources`；未配置的 Cargo 以外生态保持禁用。

## 变更文件

- `src/config.rs`
  - 增加 `SourceType`、`SourceConfig`、`SourceGraph` 和 `CargoConfig`。
  - 校验 source 名称、proxy URL、HTTPS 约束、group 成员存在性、成员顺序、
    重复成员、嵌套 group 和 self-reference。
  - Rustup service 在 Task 3 接线前通过 `pub(crate) legacy_upstream_bases()` 临时迁移
    接口读取已校验 proxy source；配置对象不再持有 `upstreams` 第二份事实。
- `src/lib.rs`
  - 导出新的配置模型和 source graph 类型。
- `tests/source_config_test.rs`
  - 增加 9 项 source graph 配置测试。
- `tests/config_test.rs`
  - 更新 `rustup.sources` schema，验证 Cargo source 和未配置 Cargo 的禁用状态。
- `tests/http_test.rs`
- `tests/service_test.rs`
  - 为新增配置字段补齐现有手工构造配置，保持既有 Rustup 行为测试有效。
- `deploy/app/config.example.toml`
  - 更新监听和根地址字段，增加 Rustup proxy/group 与 Cargo source 示例。
- `README.md`
  - 更新命名 source 配置、Rustup 命名入口和 Cargo 配置模型说明。
- `docs/architecture/rustup-mvp.md`
  - 更新配置示例、Rustup 命名路由和 source group 语义。
- `.superpowers/sdd/2026-09-08-multi-ecosystem-proxy/task-1-report.md`
  - 本报告。

## TDD 与测试输出

### RED

命令：

```text
cargo test --test source_config_test
```

结果：失败，编译器报告 `SourceType` 不存在，以及 `RustupConfig.sources` 字段不存在。
该失败对应待实现的 source 类型和 graph 配置接口。

### GREEN

命令：

```text
cargo test --test source_config_test --test config_test
```

结果：

```text
config_test: 7 passed; 0 failed
source_config_test: 7 passed; 0 failed
```

### 完整 Rust 检查

命令：

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

结果：

- `cargo fmt --all`：退出码 0。
- `cargo clippy --all-targets --all-features -- -D warnings`：退出码 0，无警告。
- `cargo test --all-targets --all-features`：35 个测试通过，0 个失败。
- `cargo build --release --locked`：release 构建成功，退出码 0。

## 未决问题

- 当前 `RustupService` 仍通过 `pub(crate) legacy_upstream_bases()` 临时迁移接口读取
  proxy source；该接口会在 Task 3 按命名入口和 group 语义接线后移除。命名入口路由、
  按 group 选择 source 以及公共 `SourcePool` 属于后续任务，Task 1 未提前改动。
- 现有工作树中其他未提交文件和既有文档计划改动未纳入本任务范围。

## 审查修正

### 问题与修复

- README 和 Rustup 架构文档恢复描述当前已实现的 `/mirror/rustup/dist/...` 路由，
  并将 `/mirror/rustup/{source}/dist/...` 与对应 `RUSTUP_DIST_SERVER` 标记为
  Task 3 目标；Task 1 不提前修改 `src/http.rs` 或 manifest 重写器。
- 移除 `RustupConfig` 的公开 `upstreams` 字段，并移除 `UpstreamConfig` 的库根导出。
  现有 service 只通过明确的内部 `legacy_upstream_bases()` 迁移接口取得临时 proxy
  URL，不再让配置对象保存两套可变事实；Task 3 必须移除该接口。
- 增加同名 source 拒绝测试。
- 增加 group 显式配置 `require_https` 的拒绝测试，并让 group 与 proxy 的
  `require_https` 字段保持互斥。

### 修正后的 TDD 与验证输出

新增 group `require_https` 测试先运行：

```text
cargo test --test source_config_test rejects_group_with_require_https
```

结果：失败，因为当前实现错误接受了 group 的 `require_https` 字段。

修正后运行：

```text
cargo test --test source_config_test --test config_test
```

结果：

```text
config_test: 7 passed; 0 failed
source_config_test: 9 passed; 0 failed
```

完整复验：

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

结果：

- `cargo fmt --all`：退出码 0。
- `cargo clippy --all-targets --all-features -- -D warnings`：退出码 0，无警告。
- `cargo test --all-targets --all-features`：35 个测试通过，0 个失败。
- `cargo build --release --locked`：release 构建成功，退出码 0。
