# Stacktory Rustup 镜像适配器 MVP

## 目标与边界

Stacktory 是公司内网研发制品和动态工具链供应平台。首期交付只实现
Rustup 动态工具链镜像适配器：开发机只访问 Stacktory 的内网地址，
Stacktory 从受信任的固定上游取得 Rust channel manifest 及其制品，并在
本地提供兼容 Rustup 的响应。

本期不实现 Cargo、npm/Bun、PyPI、NuGet、OCI、APT、YUM/DNF、Winget、
VS Code 扩展或 Unity UPM，也不承担 Python、Node.js、.NET SDK、Bun、
VS Code 等静态安装包分发。Unity 的未来适配范围仅为官方 UPM Registry
和 OpenUPM。本期不接收任意 URL，也不写入或访问生产 Nexus 配置。

## 架构

服务由一个协议路由层和若干可替换的能力组成：

| 组件 | 职责 |
| --- | --- |
| `RustupAdapter` | 识别 Rustup 原生路径；刷新、读取并重写 channel manifest；为 dist 请求构造受限制品键。 |
| `UpstreamClient` | 仅向配置中声明的上游发起请求；按顺序回退；不接受 HTTP 请求中的上游地址。 |
| `SnapshotStore` | 将已校验的原始与重写 manifest 放入版本化快照目录，并用当前指针切换公开版本。 |
| `ArtifactCache` | 以 dist 相对路径定位磁盘文件；下载到临时文件、流式计算 SHA-256、核对 manifest 哈希后才发布。 |
| `AccessGate` | 从配置的环境变量读取只读 Bearer token；未配置时仅允许显式开发模式匿名访问。 |
| `AuditSink` | 对 manifest 发布、缓存命中、缓存写入、上游回退和拒绝请求写入结构化 JSONL。 |

适配器是未来扩展点，而不是万能代理。每个生态适配器独立声明：可接受的
请求路径、上游根地址、元数据转换、制品键的语义、完整性来源和权限动作。
公共缓存只接收已经由适配器解析出的制品键和预期散列，不接收 URL。

## 路由命名空间

客户端协议路由按生态占用一级前缀。Rustup 的公开根固定为 `/rustup`，因此
开发机设置 `RUSTUP_DIST_SERVER=https://stacktory.internal/rustup` 后，Rustup
请求会自然落到 Stacktory；重写后的制品 URL 也必须从同一根生成。首期公开
路由如下：

| 路由 | 用途 |
| --- | --- |
| `GET /rustup/dist/channel-rust-{channel}.toml` | 返回当前重写后的 channel manifest。 |
| `GET /rustup/dist/channel-rust-{channel}.toml.sha256` | 返回与当前 manifest 配对的 SHA-256。 |
| `GET /rustup/dist/{path}` | 返回经过路径限制、上游校验和缓存的 Rustup 制品。 |
| `GET /healthz` | 服务存活检查，不暴露上游、缓存或配置细节。 |

管理接口预留在 `/api/v1`，例如 `POST /api/v1/rustup/refresh/{channel}`；它与
客户端镜像路由分离并要求管理权限。未来适配器使用自己的一级协议前缀，如
`/cargo`、`/npm`、`/pypi`、`/nuget`、`/oci`、`/apt`、`/yum`、`/winget`、
`/vscode` 和 `/upm`。每个前缀内部仍由对应原生协议决定，不能用全局兜底
路由把未知路径或 URL 转发到上游。

## 配置模型

配置使用 TOML，运行时仅加载一个本地文件：

```toml
listen = "127.0.0.1:18081"
data_dir = "./var/stacktory"
public_base_url = "http://127.0.0.1:18081"

[access]
# 未设置时只允许明确的开发匿名模式。
allow_anonymous = true
# read_token_env = "STACKTORY_RUSTUP_READ_TOKEN"

[[rustup.upstreams]]
name = "company-nexus"
base_url = "https://pkg.example.internal/repository/rustup-group"

[[rustup.upstreams]]
name = "rust-lang"
base_url = "https://static.rust-lang.org"
```

上游顺序就是回退顺序。所有上游 URL 必须为 `https`，不能包含查询或片段；
Rustup manifest 和制品只允许解析为相对 `dist/**` 路径。生产配置应关闭
`allow_anonymous` 并由环境变量提供 token；token 不进入版本库。

## 请求与状态流

```text
rustup
  -> GET /rustup/dist/channel-rust-{channel}.toml(.sha256)
  -> 当前快照（不存在或过期时由管理刷新触发）
  -> 重写 manifest 中全部受支持 dist URL 为 /rustup/dist/{path}
  -> 返回重写 manifest 或其 SHA-256

rustup
  -> GET /rustup/dist/{path}
  -> 严格路径解析，拒绝编码绕过、.. 和非 dist 路径
  -> 已校验缓存命中则返回
  -> 按固定上游顺序下载，SHA-256 与当前 manifest 记录相同才发布缓存
  -> 返回文件；所有失败保留为协议错误，不产生半成品缓存
```

刷新时，先从每个上游依次取得 manifest 及 `.sha256`，验证原始 manifest
散列，解析并拒绝不属于已配置上游 `dist/**` 的 URL，重写 URL 后计算新的
manifest SHA-256。完整 manifest、匹配 checksum 和提取出的制品哈希一同写入
新的版本化快照目录；目录完成后原子替换 `current.json` 指针。读取请求始终先
读取指针再读取同一快照，因此不会观察到不配对的公开 manifest/checksum。

## 错误与安全边界

请求路径不匹配或包含编码路径穿越时返回 404；认证缺失或错误返回 401；
已知但无权限的动作预留 403；没有可用快照返回 503；所有上游失败或完整性
不匹配返回 502，且审计中带有上游及失败类别。上游响应的 URL 不会被直接跟随
为任意地址：每个 URL 都须再次匹配已配置根地址和 `dist/**` 约束。

日志不记录 Bearer token、请求授权头或上游响应正文。MVP 的 JSONL 审计为
本地可追加记录；集中审计、组织/角色模型和令牌轮换是平台公共能力，后续由
`AccessGate` 与 `AuditSink` 接口替换，不改变 Rustup 协议层。

## 验证策略

单元测试覆盖配置校验、路径白名单、manifest URL 重写、散列计算和缓存发布
规则。集成测试使用本地 HTTP 模拟两个上游，验证第一个上游失败时回退、被重写
的 manifest 与 checksum 配对、首次 dist 请求缓存且第二次不访问上游、散列不符
时返回失败并且磁盘没有可命中的文件。每次 Rust 代码改动执行 `cargo fmt --all`、
`cargo clippy --all-targets --all-features -- -D warnings` 和完整测试。
