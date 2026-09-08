# Task 2 报告

## 状态

已完成公共来源访问与命名路由分发。实现范围限定为 `SourcePool`、
`HttpFetcher`、`AdapterRegistry` 和 `/mirror/{ecosystem}/{name}/{*path}` 路由；
未提前迁移 Rustup 协议，也未实现 Cargo。

## 变更文件

- `src/sources/mod.rs`
  - 注册来源池、HTTP fetcher 和适配器注册模块，并导出公共来源池接口。
- `src/sources/pool.rs`
  - 增加 `SourcePool`。
  - 通过 `SourceGraph` 解析 proxy/group entry。
  - group 按声明顺序访问成员，proxy entry 只访问自身。
  - 根据配置来源根地址和 request key 构造上游 URL，拒绝空值、查询参数、
    片段、前导斜杠、空段、`.`、`..`、反斜杠、百分号和控制字符，不对
    request key 做静默修正。
  - 将 4xx 终止错误与连接失败、重定向、5xx 可回退错误分开处理。
  - 汇总来源失败并返回成功来源名称与响应内容。
- `src/sources/fetcher.rs`
  - 增加 `HttpFetcher`。
  - 使用 30 秒请求超时和 `reqwest::redirect::Policy::none()`，不自动跟随
    未验证重定向。
  - fetcher 模块、实例和底层 fetch 方法收窄为 crate 内可见，适配器只能通过
    `SourcePool` 的受控操作访问网络。
  - 统一检查成功状态并分类请求、HTTP 状态和响应读取错误。
- `src/sources/registry.rs`
  - 增加静态 `AdapterRegistry` 和 `Adapter` 接口。
  - 将生态名称、带身份的来源池和带身份的协议适配器绑定在同一注册项中，
    拒绝重复生态和身份不匹配注册。
- `src/http.rs`
  - 增加 `router_with_registry`。
  - 增加 `/mirror/{ecosystem}/{name}/{*path}` 分发。
  - 未知生态或 source name 返回 404，剩余 path 交给适配器。
- `src/lib.rs`
  - 注册 `sources` 模块并导出 `SourcePool`。
- `tests/sources_test.rs`
  - 增加来源顺序、proxy 隔离、group 回退、request key、base path、重定向和
    4xx 终止测试。
- `tests/route_test.rs`
  - 增加生态/source name 分发、未知生态/source 的 404、重复生态和身份不匹配
    注册测试。
- `.superpowers/sdd/2026-09-08-multi-ecosystem-proxy/task-2-report.md`
  - 本报告。

## TDD 与测试输出

### RED

命令：

```text
cargo test --test sources_test --test route_test
```

结果：失败，编译器报告 `stacktory::sources`、`SourcePool` 和
`http::router_with_registry` 尚不存在。该失败对应 Task 2 待实现的来源池、
来源模块和命名路由接口。

### GREEN

命令：

```text
cargo test --test sources_test --test route_test
```

结果：

```text
route_test: 2 passed; 0 failed
sources_test: 3 passed; 0 failed
```

### 完整检查

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
- `cargo test --all-targets --all-features`：40 个测试通过，0 个失败。
- `cargo build --release --locked`：release 构建成功，退出码 0。
- `git diff --check`：退出码 0。

## Concerns

- Rustup 当前仍使用 `pub(crate) legacy_upstream_bases()` 和旧的
  `/mirror/rustup/dist/**` 路由；按用户边界保留，待 Task 3 迁移到命名入口后移除。
- 本次只提供公共来源访问和适配器注册契约，没有注册 Rustup 或 Cargo 适配器，
  因此不会伪造尚未迁移或实现的生态协议支持。
- `SourcePool` 的协议 operation 目前限定为受控 `FetchOperation::Get`；协议特有的
  内容解析和完整性校验仍由后续适配器负责。
- 用户已有的 `docs/plans/2026-08-24-rustup-mirror-mvp.md`、`src/main.rs`、
  `src/rustup/manifest.rs`、`tests/manifest_test.rs` 修改，以及其他未跟踪文档
  未纳入本次提交。

## 审查修复追加

### 重要问题修复

- `normalize_request_key` 不再剥离前导 `/`，改为结构化校验：拒绝前导 `/`、
  空 request key、空路径段、`.`、`..`、反斜杠、`%`、`?`、`#` 和控制字符；
  合法 key 保留原始相对路径，带路径的 `base_url` 仍按配置路径拼接。
- `HttpFetcher` 的模块、类型、构造器和底层 fetch 方法收窄为 crate 内可见，
  `SourcePool` 不再暴露 `fetcher()`；适配器通过 `FetchOperation::Get` 触发由
  `SourcePool` 持有的唯一 fetcher。新增本地 302 测试确认重定向目标没有被访问。
- `Adapter` 增加 `ecosystem()` 身份，`SourcePool` 保存 ecosystem 身份；
  `AdapterRegistry::with_adapter` 改为返回 `Result`，重复生态返回
  `DuplicateEcosystem`，pool 或 adapter 身份不匹配返回 `EcosystemMismatch`。
- `FetchError` 增加可回退分类：连接/响应读取失败、3xx、5xx、408 和 429 可回退；
  401、403、404 等普通 4xx 返回 `TerminalSourceError`，不访问后续来源。

### 修复 TDD 与测试输出

新增失败测试命令：

```text
cargo test --test sources_test --test route_test
```

结果：失败，编译器报告新增的 `FetchOperation`、`RegistryError`、pool ecosystem
构造参数、adapter ecosystem 接口和终止错误分支尚不存在；该失败对应审查修复
所需的公共契约与行为。

修复后 focused 测试：

```text
cargo test --test sources_test --test route_test
```

结果：

```text
route_test: 4 passed; 0 failed
sources_test: 8 passed; 0 failed
```

## 修复后完整检查

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

- `cargo fmt --all`：退出码 0。
- `cargo clippy --all-targets --all-features -- -D warnings`：退出码 0，无警告。
- `cargo test --all-targets --all-features`：42 个测试通过，0 个失败。
- `cargo build --release --locked`：release 构建成功，退出码 0。
- `git diff --check`：退出码 0。

## 修复后 concerns

- Rustup 旧协议路由和 `legacy_upstream_bases()` 仍按任务边界保留，待 Task 3
  迁移；本次没有注册 Rustup 或 Cargo 假适配器。
- 本次受控网络 API 只提供 `FetchOperation::Get`；适配器仍负责协议路径解析、
  内容解析和完整性策略，后续如需新增网络动作应扩展受控 operation，而不是重新
  暴露 `HttpFetcher` 或原始 URL。
- 用户已有未提交改动和其他未跟踪文档继续保留，未纳入本次修复提交。
