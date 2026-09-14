# Stacktory Rustup 镜像适配器 MVP

## 目标与边界

Stacktory 是公司内网研发制品和动态工具链供应平台。首期交付只实现
Rustup 动态工具链镜像适配器：开发机只访问 Stacktory 的内网地址，
Stacktory 从受信任的固定上游取得 Rust channel manifest 及其制品，并在
本地提供兼容 Rustup 的响应。

本文件只覆盖 Rustup 协议细节，不覆盖 Cargo 协议细节；Cargo 路由和 Cargo
适配边界见 `cargo-mvp.md`。npm/Bun、PyPI、NuGet、Docker、APT、YUM/DNF、Winget、
VS Code 扩展或 Unity UPM 的协议细节也不在本文件内，也不承担 Python、Node.js、
.NET SDK、Bun、VS Code 等静态安装包分发。Unity 的未来适配范围仅为官方 UPM
Registry 和 OpenUPM。本期不接收任意 URL，也不写入或访问生产 Nexus 配置。

## 架构

服务由一个协议路由层和若干可替换的能力组成：

| 组件 | 职责 |
| --- | --- |
| `RustupAdapter` | 识别 Rustup 原生路径；刷新、读取并重写 channel manifest；为 dist 请求构造受限制品键。 |
| `SourcePool` | 仅向当前命名入口解析出的 proxy 发起请求；group 按成员顺序回退，proxy 只访问自身。 |
| `SnapshotStore` | 将已校验的原始与重写 manifest 放入版本化快照目录，并用当前指针切换公开版本；快照为每个 dist 制品保存 SHA-256 和匹配的 proxy。 |
| `ArtifactCache` | 以实际 proxy 和 dist 相对路径定位磁盘文件；核对 manifest 哈希后才原子发布。 |
| `AuditSink` | 对 manifest 发布、缓存命中、缓存写入和上游失败写入结构化 JSONL。 |

适配器是未来扩展点，而不是万能代理。每个生态适配器独立声明：可接受的
请求路径、上游根地址、元数据转换、制品键的语义、完整性来源和权限动作。
公共缓存只接收已经由适配器解析出的制品键和预期散列，不接收 URL。

## 路由命名空间

Rustup 使用 `/mirror/rustup/{name}` 命名入口，开发机设置
`RUSTUP_DIST_SERVER=https://stacktory.internal/mirror/rustup/{name}` 后，Rustup
请求会落到对应 source。当前公开路由如下：

| 路由 | 用途 |
| --- | --- |
| `GET /mirror/rustup/{name}/dist/channel-rust-{channel}.toml` | 返回当前命名入口重写后的 channel manifest。 |
| `GET /mirror/rustup/{name}/dist/channel-rust-{channel}.toml.sha256` | 返回与当前 manifest 配对的 SHA-256。 |
| `GET /mirror/rustup/{name}/dist/{path}` | 返回经过路径限制、上游校验和缓存的 Rustup 制品。 |
| `GET /healthz` | 服务存活检查，不暴露上游、缓存或配置细节。 |

旧 `/mirror/rustup/dist/**` 路由不再注册，也不提供兼容别名。

MVP 不提供管理页面或管理 HTTP API。配置文件是唯一配置来源，服务器在启动时
加载它；更新配置后通过受控重启生效。其他生态的协议路由由各自适配器文档
说明；本文件不重复列出 Cargo 或其他生态的路由，也不通过全局兜底路由把未知
路径或 URL 转发到上游。

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
  -> GET /mirror/rustup/{name}/dist/channel-rust-{channel}.toml(.sha256)
  -> 当前快照新鲜则直接返回；缺失或过期时由该请求触发刷新
  -> 重写 manifest 中全部受支持 dist URL 为 /mirror/rustup/{name}/dist/{channel}/{path}
  -> 返回重写 manifest 或其 SHA-256

rustup
  -> GET /mirror/rustup/{name}/dist/{channel}/{path}
  -> 严格路径解析，拒绝编码绕过、.. 和非 dist 路径
  -> 读取当前 channel 快照中该制品的 source_name
  -> 已校验缓存命中则返回
  -> 按该制品 source_name 对应的 proxy 下载，SHA-256 与当前 manifest 记录相同才发布缓存
  -> 返回文件；所有失败保留为协议错误，不产生半成品缓存
```

刷新不是常驻任务。首次请求或快照超过 `refresh_interval_secs` 时，服务合并同一
channel 的并发刷新请求；刷新成功后发布新快照。刷新失败但已有旧快照时继续返回
旧快照，没有任何快照才返回上游失败。刷新时，按命名入口解析出的 proxy 顺序取得
manifest 及 `.sha256`，验证原始 manifest
散列，解析并拒绝不属于当前命名入口任一已配置 proxy 的 `dist/**` URL，重写 URL 后
计算新的 manifest SHA-256。完整 manifest、匹配 checksum 和每个制品的
`SHA-256 + source_name` 一同写入新的命名入口版本化快照目录；制品缓存按该
source_name 隔离，目录完成后原子替换 `current.toml` 指针。读取请求始终先读取
入口指针再读取同一快照，因此不会观察到不配对的公开 manifest/checksum；同一份
manifest 可以为不同制品绑定不同 proxy，不按每次 artifact 请求重新选择来源。
制品目录为 `data_dir/stores/rustup/artifacts/{proxy}/objects/{dist-path}`；入口名和 channel
属于 snapshot，不属于二进制制品缓存键。

direct proxy 入口只从自身获取 channel manifest；但 manifest 中的 dist URL 可以匹配
同一 Rustup 配置图内其他已声明 proxy。这样镜像 manifest 指向官方制品时仍可安全
发布，而未声明的 host 或路径始终被拒绝。

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
的 manifest 与 checksum 配对、跨 proxy manifest URL 的制品来源绑定、首次 dist
请求缓存且第二次不访问上游、散列不符时返回失败并且磁盘没有可命中的文件。每次
Rust 代码改动执行 `cargo fmt --all`、
`cargo clippy --all-targets --all-features -- -D warnings` 和完整测试。
