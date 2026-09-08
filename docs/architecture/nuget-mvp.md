# NuGet V3 restore 适配器 MVP

## 状态与目标

本文档定义 NuGet V3 restore 所需的后续只读适配边界。当前 Stacktory 尚未
实现或注册 NuGet 路由；本文档中的路径是目标契约。本 MVP 不引入数据库。

首期只服务 restore 所需的 V3 service index、registration 和 package content。
搜索、push、delete、API key、账号、符号服务器和写入型 NuGet API 保持后续
独立范围。

## 公共来源契约

每个 NuGet proxy 只配置一个上游 V3 service index JSON `base_url`，并且必须
使用 `/v3/index.json` 这一规范形态。service index、其发现的 registration、
package content 和 search 资源都从该文档发现；不拆成独立的 index URL、
registration URL、nupkg URL 或 search URL 配置。不能把 registry root 或其他
未定义的 index 形态作为第二种配置写法。

```toml
[nuget]

[[nuget.sources]]
name = "official"
type = "proxy"
base_url = "https://nuget.example/v3/index.json"
require_https = true

[[nuget.sources]]
name = "group"
type = "group"
members = ["internal", "official"]
```

`base_url` 必须是实际的 V3 service index JSON 地址
`.../v3/index.json`，而不是 registry root、管理页面地址或其他未定义形态。
适配器从该 JSON 文档读取资源的 `@type` 和 `@id`，校验并重写支持的
registration、package content 和 search resource 地址。search resource 在本
MVP 只被发现和安全校验，不注册搜索路由、不转发搜索请求；后续搜索适配必须
另行定义。group 不配置 `base_url`，只按成员顺序回退同一生态的 proxy；没有
group 时多个 proxy 不自动组合。

## 路由与适配职责

所有目标路由都使用统一命名入口：

```text
GET /mirror/nuget/{name}/v3/index.json
GET /mirror/nuget/{name}/registration/{*path}
GET /mirror/nuget/{name}/package/{id}/{version}/{id}.{version}.nupkg
```

第一个 `{name}` 是 Stacktory proxy/group 入口。NuGet package ID 的大小写按
协议比较规则处理，但原始 ID 和版本字符串必须经过路径校验，拒绝空段、路径
穿越、编码分隔符和客户端提供的外部 URL。

### Service index

适配器读取 `base_url` 指向的 service index JSON，解析资源的 `@type`、`@id`
和可选版本信息，发现 registration、package content 和 search resource。返回
客户端的支持资源 `@id` 必须重写到当前 `/mirror/nuget/{name}/...` 入口，
不能暴露上游 host；search resource 因不在本 MVP 内，可以从客户端 service
index 中省略或明确标记为不支持，不能伪装成可用搜索服务。

Service index 是 metadata。它选中的 selected proxy 必须绑定到后续 registration
和 package content 请求。group 不把两个 service index 的资源列表合并；来源
失败或资源校验失败时才按 group 顺序回退。

### Registration

适配器只读取 restore 需要的 registration index/page/leaf 资源，保留包 ID、版本、
listed/deprecation 等上游字段，不在公共层解释 NuGet 特有的版本排序和注册
语义。registration 中的 package content URL 必须重写到当前命名入口的
`/package/...` 路径，并绑定 service index 选择的 selected proxy。

若 registration 返回的 content URL 不在已校验的上游资源范围内，适配器必须
拒绝该响应，不得跟随任意 CDN 或客户端注入地址。

### Package content

package content 首期只支持 restore 所需的 `.nupkg` 下载。下载前必须由
registration 或 service index 的受限资源关系证明其属于当前 package ID/version；
下载内容写入临时文件，完成可用的摘要或内容校验后再原子发布。

如果当前 NuGet 上游未提供可用摘要，适配器可以记录本地 SHA-256 作为缓存校验
信息，但不能声称完成上游声明的完整性验证。完整性失败不得返回成功内容，也
不得发布坏缓存。

## 缓存、来源与错误

缓存键至少包含：

```text
nuget
entry_name
service-index
registration
package-content
package-id
version
representation
selected_proxy
```

service index、registration 和 package content 分别缓存。相同 package ID/version
在不同命名入口或不同 selected proxy 下不能共享没有来源绑定的文件。

- 未知入口、非法资源路径、未知 package/version 或未被 restore metadata 引用
  的 content：`404`。
- 上游明确的客户端错误直接返回，不无条件回退。
- 上游不可用、registration 无法解析、资源关系不受信任或 nupkg 完整性失败：
  按 group 顺序回退；全部失败返回 `502`。
- 本地缓存、原子写入或审计失败：`500`。

旧 metadata 在刷新失败时是否可继续服务，必须作为 NuGet 适配器的明确缓存
策略；不能因公共缓存存在就默认长期提供 stale registration。所有缓存命中、
来源回退和失败分类都写入公共审计。

## 只读范围

首期不注册或不实现：

- `Search`、`RegistrationsBaseUrl` 之外的动态搜索服务。
- `PackagePublish/2.0.0`、push、delete、unlist 和版本写入。
- API key、登录、权限、租户和账号管理。
- symbol package、符号服务器和 NuGet.org 特有的其他服务。
- 任意 URL 下载或将 service index 的未知资源自动转发。

未实现资源不应在 service index 中返回可用的假地址；未注册生态和未支持资源
应保持明确的 404 或协议错误。

## 验证边界

实现阶段使用本地 HTTP 上游覆盖：

- service index 中 registration/package content 资源发现和入口化重写。
- registration page/leaf 的读取、package ID/version 路径校验。
- selected proxy 绑定、group 回退和不合并不同来源内容。
- nupkg 摘要校验、坏包不落盘和缓存键隔离。
- 未知 service resource、外部重定向、路径穿越和写入型请求被拒绝。
- 未注册 `/mirror/nuget/{name}/...` 之外的假路由不暴露。

真实 `dotnet restore`、外部 NuGet V3 服务和生产部署行为必须在适配器实现后
单独联调。本文档不表示当前仓库已经支持 NuGet restore。

## 官方协议依据

- [NuGet API overview](https://learn.microsoft.com/en-us/nuget/api/overview)：
  V3 API 资源和 restore 相关协议入口。
- [NuGet service index](https://learn.microsoft.com/en-us/nuget/api/service-index)：
  service index、资源发现、registration 和 package content。
