# Winget REST source 只读适配器 MVP

## 状态与目标

本文档定义 Winget REST source 的只读适配边界，并记录实现时采用的协议事实、
安全决策和验证范围。目标是让内网开发机通过 Stacktory 的
`/mirror/winget/{name}/...` 入口，把该入口添加为 `Microsoft.Rest` 类型的
Winget source，完成搜索、manifest 获取和安装包下载。

当前状态：已实现并注册路由，本地 HTTP 协议测试通过；真实 Winget 客户端
联调仍需在管理员终端和受信任的 HTTPS 入口下执行。

首期只覆盖 REST source 的 `information`、`manifestSearch`、`packageManifests`
和安装包下载。source 发布、manifest 写入、账号、API key、私有认证、签名管理、
依赖解析和任意 URL 代理不在范围内。配置和缓存仍使用文件系统，不引入数据库。

## 公共来源契约

每个 Winget proxy 只配置一个上游 REST source 根 `base_url`。适配器从该根地址
按协议追加 `/information`、`/manifestSearch` 和
`/packageManifests/{PackageIdentifier}`，不增加第二个上游地址字段，也不接受
客户端提交的上游 URL。

Winget manifest 中的 `InstallerUrl` 通常指向 GitHub Releases 等第三方主机。
因此每个 Winget proxy 必须额外声明显式的安装包主机允许列表：

```toml
[winget]

[[winget.sources]]
name = "internal"
type = "proxy"
base_url = "https://winget.example/api"
installer_hosts = ["github.com", "objects.githubusercontent.com"]

[[winget.sources]]
name = "group"
type = "group"
members = ["internal"]
```

`installer_hosts` 是 Winget source 自己的安全字段，不属于通用
`RawSourceConfig`。每个 proxy 必须至少配置一个主机；group 不配置该字段。
条目只写主机名，可选 `host:port` 写法用于非默认端口。不接受 URL scheme、
路径、通配符、凭据或 query。主机比较不区分大小写。只写主机名时只匹配该
主机的默认端口；显式端口必须写进 `installer_hosts`。

安装包地址只有在以下条件全部满足时才被接受：

- URL 使用 `http` 或 `https`，且 source 未开启 `allow_http` 时必须使用
  `https`。
- URL 的主机（和显式端口）命中当前 proxy 的 `installer_hosts`。
- URL 不含凭据或 fragment，路径不含反斜杠、控制字符或 `.`/`..` 段。
- 每一跳重定向都重新执行相同的主机和 scheme 校验，最多跟随三跳。
- 允许列表中的公网主机使用会拒绝私网解析结果的客户端；显式配置的内网主机
  才走允许私网解析的路径。

未命中允许列表时，manifest 转码按来源失败处理，不会把任意 URL 返回给客户端；
安装包下载也不会发出请求。这个约束是 SSRF 边界的一部分，不能用
`base_url` 的 host 或上游响应中的 URL 临时扩展。

group 只按 `members` 顺序回退同一生态的 proxy，不合并不同来源的版本集合。
group 成员失败时沿用共享 `GroupRun` 和来源错误分类；成功后记录实际
`selected_proxy`。安装包下载使用 manifest 绑定的 selected proxy，并在来源
不可用或完整性失败时按 group 顺序尝试下一个成员。

## 路由

所有路径都包含 Stacktory 的入口名：

```text
GET  /mirror/winget/{name}/information
POST /mirror/winget/{name}/manifestSearch
GET  /mirror/winget/{name}/packageManifests/{PackageIdentifier}
GET  /mirror/winget/{name}/installers/{installer_token}
```

`{name}` 是 proxy/group 入口。`{PackageIdentifier}` 是 Winget 包标识，只允许
安全的 ASCII 路径段；空段、`.`、`..`、反斜杠、编码分隔符、控制字符和路径
穿越都返回 `404`。`manifestSearch` 只接受 JSON 对象请求体。

`information` 是 source 级握手，`manifestSearch` 是搜索，`packageManifests`
是 manifest 读取，安装包下载与它们分开处理。`information`、`manifestSearch`
和 `packageManifests` 不缓存响应正文；每次请求都经过 group 候选执行器。
安装包使用独立的 artifact 缓存和入口级 binding sidecar。

## 协议契约

### 客户端类型与信任

Winget 客户端使用 `Microsoft.Rest` source 类型访问本适配器。客户端 `source add`
在加入 REST source 时要求：

- 使用管理员终端。
- 地址以 `https://` 开头。
- 先读取 `/information`，并把返回的非空 `SourceIdentifier` 作为 source 标识。
- 如果 `information` 返回 `SourceAgreements`，显示协议并要求用户接受；如果
  没有该节点，则没有 source 级协议需要接受。

本适配器返回固定的入口级 `SourceIdentifier`，将客户端可见的
`ServerSupportedVersions` 固定为 `["1.0.0"]`，并把它作为上游请求的
`Version` 头。上游 `information` 必须支持 1.0 契约；否则适配器返回 `502`。
这是 MVP 的显式协议版本上限，不根据客户端或上游临时提升。

适配器原样转发上游 `SourceAgreements` 的 `AgreementsIdentifier`、
`AgreementLabel`、`Agreement` 和 `AgreementUrl`，不伪造协议、不替客户端接受、
也不把接受状态写入服务端。若上游声明 `Authentication`，适配器无法代替客户端
完成认证；除 `AuthenticationType = "none"` 外，按来源失败处理并返回 `502`；
私有认证不在首期范围。

`--trust-level` 是客户端本地 source 属性，不是 REST 响应的一部分。Stacktory
不会也不可能通过响应把 source 标成 `trusted`。管理员可以在确认 Stacktory
入口和上游来源后自行使用 `--trust-level trusted`；未设置时客户端仍按
`InstallerSha256` 校验安装包，只是保留客户端自己的信誉和 Mark-of-the-Web
处理。文档和提示词不把 `trusted` 写成适配器能够保证的语义。

### `information`

上游 `GET {base_url}/information` 必须返回：

```json
{
  "Data": {
    "SourceIdentifier": "upstream-source",
    "ServerSupportedVersions": ["1.0.0"]
  }
}
```

`Data.SourceIdentifier` 和 `Data.ServerSupportedVersions` 必须是非空字符串/
数组。适配器校验上游支持 1.0 后，保留 `SourceAgreements`、required/
unsupported 字段和未知字段，替换：

- `SourceIdentifier` 为 `stacktory-winget:{entry}`。
- `ServerSupportedVersions` 为 `["1.0.0"]`。

上游返回无效 JSON、缺少必需字段、声明认证或不支持 1.0 时，按来源失败处理。

### `manifestSearch`

客户端 `POST /mirror/winget/{name}/manifestSearch` 的 JSON 请求体按原样转发
到上游 `/manifestSearch`。请求体使用 Winget REST 1.0 的 `Query`、
`Filters`、`Inclusions`、`MaximumResults` 和 `FetchAllManifests` 形状，适配器
不解释或重写搜索条件。

上游响应必须是以 `Data` 数组为信封的对象。`Data` 中每个包保留
`PackageIdentifier`、`PackageName`、`Publisher` 和 `Versions`；
`Versions[].PackageVersion` 必须是非空字符串。`ContinuationToken` 和其他未知
字段原样返回。适配器只做结构校验，不合并不同来源的搜索结果，也不实现本地
搜索引擎。

### `packageManifests`

客户端 `GET /mirror/winget/{name}/packageManifests/{id}` 时，适配器把
`Version`、`Channel`、`Market` 等查询参数原样转发到上游
`/packageManifests/{id}`，并要求响应 `Data.PackageIdentifier` 与请求 ID 一致
（比较不区分大小写）。响应必须包含非空的 `Data.Versions`，每个版本包含
非空的 `PackageVersion` 和 `Installers`。

每个 installer 必须包含：

- `InstallerUrl`：绝对 `http`/`https` URL，且通过当前 proxy 的主机允许列表。
- `InstallerSha256`：64 个十六进制字符。
- `InstallerType` 和 `Architecture`：非空字符串。
- `Scope`、`InstallerLocale`：如果存在则保持原值；缺省时仍保留缺省状态。

适配器只修改 `Installers[].InstallerUrl`，把它重写为当前入口的
`/mirror/winget/{entry}/installers/{installer_token}`。`PackageIdentifier`、
`PackageVersion`、`InstallerType`、`Architecture`、`Scope`、
`InstallerLocale`、`InstallerSha256`、其他 installer 字段、locale 字段和
所有未知字段都保留。不会因为适配器不识别某个字段而删除它，也不会把未知字段
宣称为已验证语义。

`installer_token` 由入口名、包 ID、版本、`InstallerSha256`、
`InstallerType`、`Architecture`、`Scope` 和 `InstallerLocale` 共同派生；
同一内容在不同 proxy 下可以得到相同 token，但 artifact 缓存仍按实际
selected proxy 隔离。

安装包 URL 为 `/mirror/winget/{entry}/installers/{token}`。如果上游文件名
带有安全的扩展名（1–10 位字母或数字），该扩展名会保留在 URL 末尾；这样
`InstallerType: default` 仍能按扩展名判定安装器类型。无法识别扩展名时只返回
token，客户端只能依赖 manifest 中显式声明的 `InstallerType`。

如果某个 installer 的 URL 不在允许列表、缺少 SHA-256、URL 非法或版本/路径
不安全，整份 manifest 按来源失败处理，不能只删掉不支持的 installer 后返回
一个看似可用的 manifest。

## 安装包与完整性

安装包请求只接受由当前入口 manifest 生成并写入 sidecar 的 token。未建立
binding 的 token 返回 `404`，不会把 token 解码成任意 URL。sidecar 至少保存：

```text
entry_name
package_id
version
installer_token
installer_sha256
installer_type
architecture
scope
installer_locale
request_query
selected_proxy
source_fingerprint
upstream_url
content_type
```

下载流程固定为：

```text
读取入口级 binding
  -> 使用 selected proxy 或按 group 顺序选取候选来源
  -> 下载到临时文件
  -> 计算 SHA-256 并与 InstallerSha256 比较
  -> 校验成功后原子发布 artifact
  -> 更新 binding 的 selected_proxy 和来源指纹
```

hash 不匹配时删除临时文件，不发布缓存，也不向客户端返回坏字节；group 可以
继续尝试下一个来源。缓存命中仍重新计算本地 SHA-256，校验失败的文件不会被
返回。临时文件位于 `<data_dir>/tmp`，启动时清理 `winget-*.part`。

缓存键至少包含：

```text
winget
entry_name
package_id
version
installer_token / installer identity
selected_proxy
```

artifact 路径为
`<data_dir>/stores/winget/artifacts/{selected_proxy}/objects/{entry}/{package_id}/{version}/{token}`。
入口级 installer sidecar 位于
`<data_dir>/stores/winget/metadata/{entry}/installers/{token}.json`，属于来源
绑定而不是可独立过期的 refreshable metadata。

## 错误映射

- 未注册入口、非法 package ID/path、未建立 binding 的 installer token、
  所有候选来源都明确返回 `404`：`404`。
- 非法搜索 JSON 或请求语义错误：`400`。
- 上游不可用、响应无法解析、支持版本不满足、声明认证、manifest URL 未命中
  允许列表、SHA-256 不匹配或 group 全部失败：`502`。
- 本地缓存、原子写入、sidecar 或审计写入失败：`500`。
- 不支持的方法：`405`。

直接 proxy 入口不向兄弟来源回退。group 对网络失败、`401`、`403`、`404`、
`408`、`429`、`5xx` 和协议响应失败继续尝试下一个成员；其他客户端语义错误
不无条件重试。

## 只读范围

不实现 source 发布、manifest 写入、删除、账号、API key、私有认证、签名管理、
Winget 依赖解析、本地搜索服务、任意 URL 代理和客户端上传。未知路由返回
`404`，未支持的认证返回 `502`，不会生成看似可用的假响应。

## 验证边界

当前实现使用本地 axum HTTP 上游覆盖：

- `information`、`ServerSupportedVersions` 和 1.0 版本约束。
- 按包 ID 和按版本读取 manifest，查询参数转发。
- `manifestSearch` 请求体与响应信封。
- manifest 字段保真、安装器 URL 入口化重写、未知字段保留。
- 安装包 SHA-256 成功与失败、坏包不落盘、缓存命中重校验。
- 安装包主机允许列表拒绝、重定向越界拒绝。
- group 回退、selected proxy 绑定和 sidecar 更新。
- 非法包 ID、路径穿越、未注册生态和未建立 binding 的 installer。

真实 Winget 客户端联调需要管理员终端、受信任的 HTTPS 入口和可安装的测试包。
本地 HTTP 客户端测试只能证明协议适配器行为，不能替代真实 `winget source add`
和 `winget install` 验收。

## 官方协议依据

以下事实以 `microsoft/winget-cli` 当前 `master` 的源码和仓库文档为准。本文
编写时核对的提交为 `8c48efa96eec5e92ca3cb464bfd58d70f918cc2e`。

- `src/AppInstallerRepositoryCore/Rest/Schema/CommonRestConstants.h`：
  `/information`、`/manifestSearch`、`/packageManifests/` 端点，`Version`
  请求头，`Data`/`ContinuationToken` 信封，以及客户端支持的 REST 契约版本。
- `src/AppInstallerRepositoryCore/Rest/RestClient.cpp`：
  `/information` 读取、`ServerSupportedVersions` 与客户端版本的交集选择、
  `SourceAgreements` 和认证信息进入 source information。
- `src/AppInstallerRepositoryCore/Rest/Schema/InformationResponseDeserializer.cpp`：
  `SourceIdentifier`、`ServerSupportedVersions`、`SourceAgreements` 和
  required/unsupported 字段的准确 JSON 名称与结构。
- `src/AppInstallerRepositoryCore/Rest/Schema/1_0/RestInterface_1_0.cpp`：
  `manifestSearch` 的 POST、`packageManifests/{id}` 的 GET，以及
  `Version`/`Channel`/`Market` 查询参数和搜索结果/manifest 读取流程。
- `src/AppInstallerRepositoryCore/Rest/Schema/1_0/Json/SearchRequestSerializer_1_0.cpp`
  和 `SearchResponseDeserializer_1_0.cpp`：
  搜索请求的 `Query`/`Filters`/`Inclusions`/`MaximumResults` 形状和搜索响应
  的 `Data` 数组、包字段、版本字段。
- `src/AppInstallerRepositoryCore/Rest/Schema/1_0/Json/ManifestDeserializer_1_0.cpp`：
  `Data`、`PackageIdentifier`、`Versions`、`PackageVersion`、`DefaultLocale`、
  `Installers`、`InstallerUrl`、`InstallerSha256`、`InstallerType`、
  `Architecture`、`Scope`、`InstallerLocale` 等 manifest JSON 名称和必填性。
- `src/AppInstallerRepositoryCore/Rest/RestSourceFactory.cpp`：
  `Microsoft.Rest` source 添加时必须使用远程且安全的 `https://` 地址。
- `src/AppInstallerCLICore/Workflows/PromptFlow.cpp` 和
  `src/AppInstallerRepositoryCore/SourceList.cpp`：
  `SourceAgreements` 的客户端提示、接受标识和本地保存语义；没有协议时
  `CheckSourceAgreements` 直接视为已满足。
- `src/AppInstallerCLICore/Workflows/DownloadFlow.cpp`：
  source `TrustLevel` 对安装包信任标记、MotW 和信誉检查的客户端本地影响。
- `doc/windows/package-manager/winget/source.md`：
  客户端 source 子命令、`Microsoft.Rest` 类型和 `winget source add` 的用法。

仓库当前没有单独的 REST API Markdown 规范文件；上述 schema 实现和客户端
文档是本次核对的规范性一手依据，不使用未在这些文件中出现过的端点和字段。
