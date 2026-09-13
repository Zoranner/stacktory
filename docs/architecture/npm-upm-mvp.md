# npm 与 Unity UPM 适配器 MVP

## 状态与目标

本文档定义 npm 和 Unity Package Manager（UPM）的只读适配边界。npm 和 UPM 的
metadata、tarball、校验、缓存和来源回退均已实现，但使用独立生态入口和名称校验。

npm 与 UPM 都使用 npm-compatible registry 的 metadata、版本对象和 tarball 文件模型。npm 客户端包括 npm 和 Bun；两者共享协议解析、来源访问、缓存、完整性和 URL 重写的核心实现，但保持独立的生态入口和客户端配置语义。UPM 不能通过复制一套 npm handler 实现，也不能把 npm 入口扩展成隐式 UPM 模式。

## 公共来源契约

每个生态单独配置 `sources`。proxy 只有一个 `base_url`，它是该上游 registry
根地址；metadata 和 tarball 都从这个根地址按 npm-compatible 路径访问。
proxy 不配置第二个 metadata 地址、tarball 地址或 scope 地址。

```toml
[npm]

[[npm.sources]]
name = "official"
type = "proxy"
base_url = "https://registry.example"
require_https = true

[[npm.sources]]
name = "group"
type = "group"
members = ["internal", "official"]

[upm]

[[upm.sources]]
name = "official"
type = "proxy"
base_url = "https://upm.example"
require_https = true

[[upm.sources]]
name = "group"
type = "group"
members = ["internal", "official"]
```

没有 group 时，多个 proxy 彼此独立，不自动组合或回退。group 只按成员顺序
引用同一生态的 proxy，不配置 `base_url`，也不引用另一个 group。

## npm 路由与协议边界

目标路由统一使用：

```text
/mirror/npm/{name}/...
```

其中第一个 `{name}` 是 Stacktory 的 proxy/group 入口，不是 npm package name。
适配器在其后解析普通或 scoped package 路径：

```text
GET /mirror/npm/{name}/{package}
GET /mirror/npm/{name}/@scope%2F{package}
GET /mirror/npm/{name}/{package}/{version}
GET /mirror/npm/{name}/-/package/{package}/dist-tags
GET /mirror/npm/{name}/-/v1/search
GET /mirror/npm/{name}/-/ping
GET /mirror/npm/{name}/tarballs/{artifact-key}
```

版本查询、dist-tags、search 和 ping 是只读查询接口：search 仅透传官方支持的查询
参数，版本与 dist-tags 从已重写的 metadata 派生，不新增缓存表示。

上面的 `<package>`、`<version>` 和 scoped package 路径只是协议对象键；实现必须
按 npm package name 规则校验路径。scoped metadata 的原始请求路径只允许
`@scope%2Fpackage` 中作为 scope 分隔符的 `%2F`；适配器先按原始路径识别这一
特例，再分别校验 scope 和 package。其他编码斜杠、空段、路径穿越和客户端
指定的 URL 一律拒绝。

首期适配器读取 npm registry metadata，并保留客户端 restore/install 所需的
package name、版本对象、dist 信息和其他未禁止的 metadata 字段。版本对象中的
tarball 地址必须重写为当前入口的
`/mirror/npm/{name}/tarballs/{artifact-key}` 路径，不向客户端暴露上游 registry
地址。`artifact-key` 由适配器从 metadata 的 dist 记录生成并绑定当前版本和
selected proxy，不复用客户端传入的 tarball 路径。`dist.integrity` 或可用的
`dist.shasum` 是制品校验期望值；缺少上游摘要时只能记录本地摘要，不能声称
完成上游完整性证明。

metadata 命中某个 proxy 后，版本对象和 tarball 请求必须保留
`selected_proxy` 绑定。group 不合并多个 proxy 的版本集合；只有当前来源不可用
或校验失败时才按声明顺序回退。

## UPM 路由与本地 scope 配置

UPM 使用独立生态入口：

```text
/mirror/upm/{name}/...
```

UPM 入口提供与 npm 相同的只读查询（package metadata、指定版本、dist-tags、search、
ping）和 tarball 下载，但使用 Unity package name 校验。

UPM 的 registry 传输复用 npm-compatible metadata 和 tarball 核心，但 Unity 客户端
通过项目 `Packages/manifest.json` 中的 `scopedRegistries` 决定哪些 package scope
由该入口提供。scope 是用户本地配置，不属于 Stacktory source graph，也不改变
`{name}` 的含义。

UPM package name 通常是 `com.company.package` 形式。适配器按 Unity package name
规则校验；不能把 UPM 包强制转换成 npm scoped package，也不能让 UPM 请求落入
`/mirror/npm/{name}/...`。

## 共享核心与隔离点

可以共享：

- npm-compatible metadata 的 JSON 解析和未知字段保留策略。
- 版本对象中 dist/tarball 的地址重写。
- tarball 临时下载、摘要校验和原子发布。
- source/group 解析、来源绑定、缓存 TTL 和审计字段。

必须独立：

- `/mirror/npm/{name}/...` 与 `/mirror/upm/{name}/...` 路由注册。
- npm package name 与 UPM Unity package name 的路径校验。
- UPM package name 校验；scope 选择由 Unity 客户端本地配置负责。
- npm 与 UPM 的缓存键生态字段、协议变体和测试夹具。

“共享核心”指实现层的受限协议组件，不是把两种客户端语义合并为一个万能
入口。任何 UPM 特有能力都必须在 UPM 适配器中显式声明。

## 缓存、错误与只读边界

npm metadata 和 UPM metadata 是 metadata 缓存；tarball 是 artifact 缓存。缓存
键至少包含 ecosystem、entry name、协议变体、package object key、响应表示
（`representation`）
和 selected proxy，避免 npm 与 UPM 或不同命名入口互相命中。

非法 package path、未被 metadata 引用的 tarball 和未知入口返回 `404`。上游
明确的客户端错误不无条件回退；上游不可用、metadata 无法解析或 tarball
完整性失败允许 group 尝试下一个 proxy。所有来源失败返回 `502`，本地存储
失败返回 `500`。失败内容不得发布到缓存。

本 MVP 不引入数据库，不接受任意 URL，不实现 publish、unpublish、dist-tag
修改、账号、token、权限管理或任何包写入 API。UPM 也不实现 Unity Editor、
Asset Store 和 Git URL 包代理。

## 验证边界

实现阶段使用本地 HTTP 上游覆盖：

- 普通 package 和 scoped/npm package 路径。
- `%2F` scoped metadata 分隔符的原始路径解析，以及其他编码斜杠拒绝。
- UPM package name 和本地 scoped registry 配置。
- metadata 中 tarball URL 的入口化重写。
- `integrity`/`shasum` 校验、坏包不落盘和 group 回退。
- npm/UPM 入口与缓存键隔离。
- 未注册路由不暴露假支持。

真实 npm CLI、Unity Package Manager 和外部 registry 联调属于实现交付后的
独立验证，不能由本文档或公共 Rust 测试代替。

## 官方协议依据

- [npm Registry API](https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md)：
  package metadata、版本对象和 tarball 字段。
- [Unity Manual: Scoped registries](https://docs.unity3d.com/Manual/upm-scoped.html)：
  UPM registry 与 package scope 配置。
