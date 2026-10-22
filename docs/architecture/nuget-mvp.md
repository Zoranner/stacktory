# NuGet V3 restore 适配器 MVP

## 状态与目标

本文档定义并记录已实现的 NuGet V3 只读适配边界。当前已完成 service index、Search、
package version index、registration、package content、漏洞数据、来源绑定、缓存、
审计和运行时注册；真实外部 NuGet 服务与 `dotnet restore` 已完成验证。本 MVP 不
引入数据库。

首期服务 V3 service index、Search、registration、package content 和漏洞数据等只读
能力。push、delete、API key、账号、符号服务器和写入型 NuGet API 保持后续独立范围。

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
allow_http = false

[[nuget.sources]]
name = "group"
type = "group"
members = ["internal", "official"]
```

`base_url` 必须是实际的 V3 service index JSON 地址
`.../v3/index.json`，而不是 registry root、管理页面地址或其他未定义形态。
适配器从该 JSON 文档读取资源的 `@type` 和 `@id`，校验并重写支持的
registration、package content、search 和 `VulnerabilityInfo/6.7.0` resource
地址。发现到的资源只通过当前 `/mirror/nuget/{name}/...` 入口对外提供，客户端
不能通过查询参数或路径补充外部 URL。group 不配置 `base_url`，只按成员顺序
回退同一生态的 proxy；没有 group 时多个 proxy 不自动组合。

## 路由与适配职责

所有目标路由都使用统一命名入口：

```text
GET /mirror/nuget/{name}/v3/index.json
GET /mirror/nuget/{name}/search/query?{query}
GET /mirror/nuget/{name}/package/{id}/index.json
GET /mirror/nuget/{name}/registration/{*path}
GET /mirror/nuget/{name}/package/{id}/{version}/{id}.{version}.nupkg
GET /mirror/nuget/{name}/vulnerability/index.json
GET /mirror/nuget/{name}/vulnerability/data/{file}.json
```

第一个 `{name}` 是 Stacktory proxy/group 入口。NuGet package ID 的大小写按
协议比较规则处理，但原始 ID 和版本字符串必须经过路径校验，拒绝空段、路径
穿越、编码分隔符和客户端提供的外部 URL。

### Service index

适配器读取 `base_url` 指向的 service index JSON，解析资源的 `@type`、`@id`
和可选版本信息，发现 registration、package content、search 和漏洞数据
resource。返回客户端的支持资源 `@id` 必须重写到当前 `/mirror/nuget/{name}/...`
入口，不能暴露上游 host；SearchQueryService 只允许发现和安全转发查询参数，
不接受客户端提供的外部 URL。

Service index 是 metadata。它选中的 selected proxy 必须绑定到后续 registration
和 package content 请求。group 不把两个 service index 的资源列表合并；来源
失败或资源校验失败时才按 group 顺序回退。

当上游同时声明多个 `RegistrationsBaseUrl` 别名时，适配器按 NuGet 客户端的
偏好顺序（`/Versioned`、`/3.6.0` 优先）选择一个对外暴露，避免退回信息较少的
semver1 registration；未选中的别名地址只参与 search 结果的 registration 链接
重写。

### Registration

适配器只读取 restore 需要的 registration index/page/leaf 资源，保留包 ID、版本、
listed/deprecation 等上游字段，不在公共层解释 NuGet 特有的版本排序和注册
语义。registration 中的 package content URL 必须重写到当前命名入口的
`/package/...` 路径，并绑定 service index 选择的 selected proxy。

若 registration 返回的 content URL 不在已校验的上游资源范围内，适配器必须
拒绝该响应，不得跟随任意 CDN 或客户端注入地址。

registration 文档中的 JSON-LD `@context` 属于术语定义，不参与重写；`catalogEntry`
或对象自身的 `id`/`version` 必须能证明 package content 的归属。无法入口化的
内嵌 `@id`（catalog、dependency group 标识）会被丢弃，`parent`/`registration`
等外链指针校验失败时整份响应按来源失败处理。

### Package content

package content 首期只支持 restore 所需的 `.nupkg` 下载。下载前必须由
registration 或 service index 的受限资源关系证明其属于当前 package ID/version；
下载内容写入临时文件，完成可用的摘要或内容校验后再原子发布。
缓存命中从已校验的文件流式返回，不把整个包读入内存；服务启动时会清理遗留的
`nuget-*.part` 临时文件，避免异常退出后长期累积。

如果当前 NuGet 上游未提供可用摘要，适配器可以记录本地 SHA-256 作为缓存校验
信息，但不能声称完成上游声明的完整性验证。完整性失败不得返回成功内容，也
不得发布坏缓存。

### 漏洞数据

`VulnerabilityInfo/6.7.0` 提供 NuGet 审计数据，`dotnet restore` 的 NuGetAudit 和
`dotnet list package --vulnerable` 都读取该资源。适配器只暴露上游 service index
声明的 `/vulnerability/index.json`，以及该索引声明的
`/vulnerability/data/{file}.json`；索引中的文件地址必须与当前 proxy 同源并通过
安全校验，客户端自造的未声明文件名返回 `404`。

漏洞索引和数据文件分别缓存并绑定 selected proxy；group 按成员顺序回退，但
不合并不同来源的漏洞集合。数据文件只有解析为 JSON 对象且每个值都是数组时才
写入缓存。

## 缓存、来源与错误

缓存键至少包含：

```text
nuget
entry_name
service-index
registration
package-content
vulnerability-index
vulnerability-data
package-id
version
representation
selected_proxy
```

service index、registration、package content、漏洞索引和漏洞数据分别缓存。相同
package ID/version 在不同命名入口或不同 selected proxy 下不能共享没有来源绑定的
文件。

service index、registration、package binding 和漏洞索引/数据的 sidecar 记录
来源指纹，由 proxy 名称、`base_url` 和 `allow_http` 组成。同名 proxy 修改上游
目标后，旧 metadata 按未命中重新获取；内容寻址且摘要仍匹配的 package content
可以继续复用。`public_url` 不进入来源指纹，公开地址变化时 adapter 只重写 JSON 中的
客户端可见 URL 并更新 sidecar，不访问上游，也不删除 package content。缺少来源
指纹或公开基地址标记的旧缓存按未命中重新获取。

service index、registration、package version index 和 package binding 命中超过
15 分钟后会先从当前绑定来源重新验证；来源失败时按 group 顺序尝试其他成员。
重新验证成功时替换 metadata、来源绑定和 `validated_at_unix_secs`；上游返回
`404` 时删除对应 metadata 和绑定；网络、`5xx` 或其他协议失败时继续提供最后一次
有效缓存，并记录 stale 审计。类型不同来源的 metadata 不合并。

package content 缓存每次命中仍执行已声明摘要或本地 SHA-256 校验。package binding
过期后重新解析 registration 或 package index；上游提供摘要时，重发版本的新摘要
会使旧 content 缓存失效并重新下载。上游未提供摘要且 metadata 内容没有变化时，
服务无法仅凭协议判断同一版本内容已被重发，需要人工清理对应 package content。
也可以强制重建指定入口的 NuGet metadata：

```text
stacktory invalidate-nuget-metadata --entry <name>
```

命令只删除该入口的 metadata 和来源绑定，保留 package content；新 registration
提供摘要时按新摘要重新校验。上游未提供摘要且来源指纹未变化时，该命令不会使旧
package content 自动失效，需要另行清理对应的 package content 缓存。

- 未知入口、非法资源路径、未知 package/version、未被 restore metadata 引用的
  content 或未被漏洞索引声明的文件名：`404`。
- 上游明确的客户端错误直接返回，不无条件回退。
- 上游不可用、registration 无法解析、资源关系不受信任（包括漏洞文件地址越界）
  或 nupkg 完整性失败：按 group 顺序回退；全部失败返回 `502`。
- 本地缓存、原子写入或审计失败：`500`。

旧 metadata 在刷新失败时是否可继续服务，必须作为 NuGet 适配器的明确缓存
策略；不能因公共缓存存在就默认长期提供 stale registration。所有缓存命中、
来源回退和失败分类都写入公共审计。

## 只读范围

不注册或不实现：

- `SearchAutocompleteService`、`Catalog`、`RepositorySignatures` 和展示类 URI 模板。
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
- 漏洞索引重写、未声明文件名、越界文件地址拒绝和 group 回退。
- 未知 service resource、外部重定向、路径穿越和写入型请求被拒绝。
- 未注册 `/mirror/nuget/{name}/...` 之外的假路由不暴露。

真实客户端已使用官方 NuGet 源分别验证 `dotnet restore`（含传递依赖和
NuGetAudit）、`dotnet list package --vulnerable/--outdated/--deprecated` 和
`dotnet package search`；生产部署行为仍需按部署环境单独联调。

## 官方协议依据

- [NuGet API overview](https://learn.microsoft.com/en-us/nuget/api/overview)：
  V3 API 资源和 restore 相关协议入口。
- [NuGet service index](https://learn.microsoft.com/en-us/nuget/api/service-index)：
  service index、资源发现、registration 和 package content。
- [NuGet vulnerability info](https://learn.microsoft.com/en-us/nuget/api/vulnerability-info)：
  漏洞索引和数据文件格式。
