# Stacktory Rustup 镜像适配器 MVP

## 目标与边界

Stacktory 是公司内网研发制品和动态工具链供应平台。首期交付只实现
Rustup 动态工具链镜像适配器：开发机只访问 Stacktory 的内网地址，
Stacktory 从受信任的固定上游取得 Rust channel manifest 及其制品，并在
本地提供兼容 Rustup 的响应。

本期不实现 Cargo 协议路由、npm/Bun、PyPI、NuGet、OCI、APT、YUM/DNF、Winget、
VS Code 扩展或 Unity UPM，也不承担 Python、Node.js、.NET SDK、Bun、
VS Code 等静态安装包分发。Unity 的未来适配范围仅为官方 UPM Registry
和 OpenUPM。本期不接收任意 URL，也不写入或访问生产 Nexus 配置。

## 架构

服务由一个协议路由层和若干可替换的能力组成：

| 组件 | 职责 |
| --- | --- |
| `RustupAdapter` | 识别 Rustup 原生路径；刷新、读取并重写 channel manifest；为 dist 请求构造受限制品键。 |
| `SourcePool` | 仅向当前命名入口解析出的 proxy 发起请求；group 按成员顺序回退，proxy 只访问自身。 |
| `SnapshotStore` | 将已校验的原始与重写 manifest 放入版本化快照目录，并用当前指针切换公开版本。 |
| `ArtifactCache` | 以 dist 相对路径定位磁盘文件；核对 manifest 哈希后才原子发布。 |
| `AuditSink` | 对 manifest 发布、缓存命中、缓存写入和上游失败写入结构化 JSONL。 |

适配器是未来扩展点，而不是万能代理。每个生态适配器独立声明：可接受的
请求路径、上游根地址、元数据转换、制品键的语义、完整性来源和权限动作。
公共缓存只接收已经由适配器解析出的制品键和预期散列，不接收 URL。

## 路由命名空间

Rustup 使用 `/mirror/rustup/{source}` 命名入口，开发机设置
`RUSTUP_DIST_SERVER=https://stacktory.internal/mirror/rustup/{source}` 后，Rustup
请求会落到对应 source。当前公开路由如下：

| 路由 | 用途 |
| --- | --- |
| `GET /mirror/rustup/{source}/dist/channel-rust-{channel}.toml` | 返回当前命名入口重写后的 channel manifest。 |
| `GET /mirror/rustup/{source}/dist/channel-rust-{channel}.toml.sha256` | 返回与当前 manifest 配对的 SHA-256。 |
| `GET /mirror/rustup/{source}/dist/{path}` | 返回经过路径限制、上游校验和缓存的 Rustup 制品。 |
| `GET /healthz` | 服务存活检查，不暴露上游、缓存或配置细节。 |

旧 `/mirror/rustup/dist/**` 路由不再注册，也不提供兼容别名。

MVP 不提供管理页面或管理 HTTP API。配置文件是唯一配置来源，服务器在启动时
加载它；更新配置后通过受控重启生效。未来路由使用 `/mirror/cargo`、
`/mirror/npm`、`/mirror/pypi`、`/mirror/nuget`、`/mirror/oci`、`/mirror/apt`、
`/mirror/yum`、`/mirror/winget`、`/mirror/vscode` 和 `/mirror/upm` 等前缀。
每个生态前缀内部仍由对应原生协议决定，不能用全局兜底路由把未知路径或 URL
转发到上游。

官网和文档是后续独立交付的只读用户界面：`/` 展示 Stacktory 产品入口，
`/docs/{stack}` 提供各技术栈的配置、使用和排障教程，例如
`/docs/rustup`。它们可由同域反向代理到静态站点或独立前端，但不得访问服务的
配置文件、缓存目录或任何配置修改能力，也不能占用已分配的协议前缀。当前
Rustup MVP 只保留这些路由空间，不实现官网或文档站。

## 配置模型

配置使用 TOML，运行时仅加载一个本地文件：

```toml
listen_address = "127.0.0.1"
listen_port = 18761
data_dir = "./var/stacktory"
root_url = "http://127.0.0.1:18761"

[rustup]
refresh_interval_secs = 3600
channels = ["stable", "beta", "nightly"]

[[rustup.sources]]
name = "company-nexus"
type = "proxy"
base_url = "https://pkg.example.internal/repository/rustup-group"

[[rustup.sources]]
name = "rust-lang"
type = "proxy"
base_url = "https://static.rust-lang.org"

[[rustup.sources]]
name = "group"
type = "group"
members = ["company-nexus", "rust-lang"]

[cargo]

[[cargo.sources]]
name = "crates-io"
type = "proxy"
base_url = "https://crates.io"
```

上游顺序就是回退顺序。`channels` 是服务允许刷新并可供给制品哈希的唯一 channel
集合，必须至少包含一项；`refresh_interval_secs` 是 manifest 重新校验间隔，必须为
正整数。上游 URL 只能使用 `https` 或 `http`，且不能包含
凭据、查询或片段；上游默认 `require_https = true`，HTTP 上游必须在该条目显式
设置 `require_https = false`。这个开关不接受客户端输入，也不允许服务访问未在
配置中声明的地址。Rustup manifest 和制品只允许解析为相对 `dist/**` 路径。生产
Stacktory 对内网公开提供镜像响应，不要求客户端凭据。

## 请求与状态流

```text
rustup
  -> GET /mirror/rustup/{source}/dist/channel-rust-{channel}.toml(.sha256)
  -> 当前快照新鲜则直接返回；缺失或过期时由该请求触发刷新
  -> 重写 manifest 中全部受支持 dist URL 为 /mirror/rustup/{source}/dist/{channel}/{path}
  -> 返回重写 manifest 或其 SHA-256

rustup
  -> GET /mirror/rustup/{source}/dist/{channel}/{path}
  -> 严格路径解析，拒绝编码绕过、.. 和非 dist 路径
  -> 已校验缓存命中则返回
  -> 按该命名入口 manifest 选中的 proxy 下载，SHA-256 与当前 manifest 记录相同才发布缓存
  -> 返回文件；所有失败保留为协议错误，不产生半成品缓存
```

刷新不是常驻任务。首次请求或快照超过 `refresh_interval_secs` 时，服务合并同一
channel 的并发刷新请求；刷新成功后发布新快照。刷新失败但已有旧快照时继续返回
旧快照，没有任何快照才返回上游失败。刷新时，按命名入口解析出的 proxy 顺序取得
manifest 及 `.sha256`，验证原始 manifest
散列，解析并拒绝不属于已配置上游 `dist/**` 的 URL，重写 URL 后计算新的
manifest SHA-256。完整 manifest、匹配 checksum 和提取出的制品哈希一同写入
新的命名入口版本化快照目录；快照同时记录实际选中的 proxy，制品缓存按 proxy
隔离，目录完成后原子替换
`current.toml` 指针。读取请求始终先读取入口指针再读取同一快照，因此不会观察到
不配对的公开 manifest/checksum；artifact 请求沿用该快照记录的 proxy，不回退到
该 group 的其他成员。

## 错误与安全边界

请求路径不匹配或包含编码路径穿越时返回 404；没有可用快照返回 503；所有上游失败
或完整性不匹配返回 502，且审计中带有失败类别。上游响应的 URL 不会被直接跟随
为任意地址：每个 URL 都须再次匹配已配置根地址和 `dist/**` 约束。

日志不记录上游响应正文。MVP 的 JSONL 审计为本地可追加记录；集中审计和组织/角色
模型是平台公共能力，后续可在不改变 Rustup 协议层的前提下接入。

## 验证策略

单元测试覆盖配置校验、路径白名单、manifest URL 重写、散列计算和缓存发布
规则。集成测试使用本地 HTTP 模拟两个上游，验证第一个上游失败时回退、首次和
过期 manifest 请求触发刷新、被重写
的 manifest 与 checksum 配对、首次 dist 请求缓存且第二次不访问上游、散列不符
时返回失败并且磁盘没有可命中的文件。每次 Rust 代码改动执行 `cargo fmt --all`、
`cargo clippy --all-targets --all-features -- -D warnings` 和完整测试。
