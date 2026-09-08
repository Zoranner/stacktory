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
    片段和父级路径段。
  - 汇总来源失败并返回成功来源名称与响应内容。
- `src/sources/fetcher.rs`
  - 增加 `HttpFetcher`。
  - 使用 30 秒请求超时和 `reqwest::redirect::Policy::none()`，不自动跟随
    未验证重定向。
  - 统一检查成功状态并分类请求、HTTP 状态和响应读取错误。
- `src/sources/registry.rs`
  - 增加静态 `AdapterRegistry` 和 `Adapter` 接口。
  - 将生态名称、来源池和协议适配器绑定在同一注册项中。
- `src/http.rs`
  - 增加 `router_with_registry`。
  - 增加 `/mirror/{ecosystem}/{name}/{*path}` 分发。
  - 未知生态或 source name 返回 404，剩余 path 交给适配器。
- `src/lib.rs`
  - 注册 `sources` 模块并导出 `SourcePool`。
- `tests/sources_test.rs`
  - 增加来源顺序、proxy 隔离和 group 回退测试。
- `tests/route_test.rs`
  - 增加生态/source name 分发及未知生态、未知 source 的 404 测试。
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
- `SourcePool` 的 operation 负责具体协议请求和响应处理；当前公共层对 operation
  返回的上游错误按来源顺序继续尝试，协议特有的客户端错误和完整性校验策略
  应在后续适配器接入时明确分类。
- 用户已有的 `docs/plans/2026-08-24-rustup-mirror-mvp.md`、`src/main.rs`、
  `src/rustup/manifest.rs`、`tests/manifest_test.rs` 修改，以及其他未跟踪文档
  未纳入本次提交。
