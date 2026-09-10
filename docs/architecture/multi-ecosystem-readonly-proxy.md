# Stacktory 多生态只读代理架构设计

## 目标

Stacktory 为内网开发机提供统一的研发制品访问入口。当前实现包含 Rustup
动态镜像和 Cargo Sparse Index/crate 只读代理；npm、UPM、PyPI、NuGet、Docker、
VS Code 扩展和 Winget
只在本文件及各自 MVP 文档中定义后续适配边界，未实现的生态不注册路由。

本设计约束公共来源访问、显式组合、缓存、完整性和审计职责，使新增生态只
实现协议适配器，不复制公共行为。本设计不引入数据库，不提供写入型包管理
能力，也不接受客户端传入任意上游 URL。

## 范围

### 本期保留能力

- Rustup 命名入口和动态 manifest 镜像。
- Cargo Sparse Index 与 crate 下载。
- 公共 `sources` 配置、`proxy`/`group` source graph、来源回退、文件缓存、
  原子写入、完整性校验和 JSONL 审计。
- 统一路由模型：

  ```text
  /mirror/{ecosystem}/{name}/...
  ```

### 后续适配边界

- npm：package metadata、scoped package metadata 和 tarball 下载。
- UPM：独立的 UPM 生态入口，复用 npm-compatible metadata/tarball 核心，
  增加 scope 选择规则。
- PyPI：Simple API 的 HTML/JSON 表示和项目文件下载。
- NuGet：V3 service index、registration 和 package content。
- Docker：Registry V2 manifest、image config、layer blob、tag/digest 拉取和
  多架构 manifest。
- VS Code 扩展：扩展 metadata、版本信息和 `.vsix` 下载。
- Winget：source/index、package manifest 和安装包下载。

### 明确不在范围内

- npm publish、unpublish、账号和 token 管理。
- PyPI 上传、删除、账号和 token 管理。
- NuGet push、delete、搜索、API key、账号和动态注册服务。
- Docker 镜像推送、删除、账号、签名管理和构建服务。
- VS Code 扩展发布、删除、账号和 token 管理。
- Winget source 发布、manifest 写入、账号和签名管理。
- UPM 发布、包写入、Unity Editor、Asset Store 和 Git URL 包。
- 管理后台、动态修改配置、远程配置中心和数据库。
- 任意 URL 代理、客户端指定上游、未验证的跨来源重定向。

## 公共来源契约

### Proxy 与 group

每个生态有独立的 `sources` 列表。`proxy` 是一个命名上游入口，且只能配置
一个 `base_url`；协议需要的 metadata、index、registration 和 artifact
路径都从这个根地址按该生态规则生成，不拆成多个上游地址字段。

```toml
[[<ecosystem>.sources]]
name = "official"
type = "proxy"
base_url = "https://registry.example"
require_https = true
```

`group` 是显式组合入口，不配置 `base_url`，只按 `members` 顺序引用同一生态
中已经声明的 proxy：

```toml
[[<ecosystem>.sources]]
name = "group"
type = "group"
members = ["internal", "official"]
```

没有 group 时，多个 proxy 彼此独立，不自动组合、不自动回退。group 首期只能
引用 proxy；成员非空、不能重复、必须存在且属于同一生态。禁止 group 嵌套和
自引用，避免把循环检测和递归展开引入公共 MVP。

`name` 只需在同一生态内唯一，同时作为路由路径段、日志标识和审计字段。它
不能为空，只允许 ASCII 字母、数字、`.`、`_` 和 `-`；不接受编码斜杠、路径
分隔符或控制字符。`default_source` 不存在，客户端请求中的 `{name}` 是唯一
的入口选择。

### 来源绑定

来源绑定使用两级文件系统模型，不引入数据库：

```text
稳定 lookup key
  -> metadata current + metadata sidecar
       sidecar.selected_proxy
       sidecar.integrity / representation / protocol_variant
  -> artifact object
```

稳定 lookup key 至少由 ecosystem、命名入口、协议变体、对象键和响应表示组成，
不由上游 URL 临时决定。metadata 的 `current` 文件保存对客户端可见的当前
表示，sidecar 保存其来源绑定和完整性信息；两者必须以同一次原子发布产生，不能
让 current 与 sidecar 指向不同内容。

一次 metadata/index/registration 成功响应必须记录实际选中的 proxy。适配器
重写制品链接时，链接仍指向当前 Stacktory 命名入口，但 artifact 请求必须先
读取对应 metadata sidecar 的 `selected_proxy`，再访问该 proxy。group 不能在
没有绑定的情况下按每次请求重新选择来源并拼接结果。

artifact 请求只有在绑定来源不可用、响应无法使用或完整性校验失败时，才允许按
已声明的 group 顺序尝试下一个 proxy。回退得到的制品必须先通过 metadata 或
协议声明的摘要校验，再原子发布 artifact，并更新 sidecar 的
`selected_proxy`；校验前不得更新绑定。协议没有可验证摘要时，不得把“下载
成功”当作切换来源的依据，具体适配器必须明确禁止该回退或采用更强的协议证明。

不允许把不同 proxy 的版本集合、registration 或项目索引合并成一个响应。这样
可以保持 metadata 与 artifact 的来源一致性，并让审计能够复现来源选择。

### 单一 Base URL 与安全

proxy 的 `base_url` 是服务端配置事实，不是客户端可覆盖的转发目标。它必须
通过 scheme、host、凭据、query、fragment、路径和 `require_https` 校验。适配器
只能用协议对象键拼接该根地址；上游响应中的链接也必须经过协议范围和地址
校验后才能访问。禁止 loopback、private、link-local、unspecified 和明显内部
域名的外部跳转，除非后续安全设计明确允许并增加独立验证。

## 路由与适配器

所有生态的对外协议根都使用：

```text
/mirror/{ecosystem}/{name}/...
```

其中 `{ecosystem}` 是已注册生态，`{name}` 是该生态中的 proxy 或 group 名称。
后续路径完全由适配器解析。例如：

```text
/mirror/rustup/group/dist/...
/mirror/cargo/group/config.json
/mirror/npm/group/<package>
/mirror/upm/group/<package>
/mirror/pypi/group/simple/<project>/
/mirror/nuget/group/v3/index.json
```

未知生态、未知入口或未注册的生态路由返回 404。路由层只解析生态和入口，不
理解包名、版本、index、manifest 或 registration 语义。

适配器独立负责：

- 识别协议路径并生成受限对象键。
- 使用一个 proxy 的 `base_url` 生成上游请求。
- 解析和重写 metadata 中的客户端可见 URL。
- 提供可验证的完整性期望值。
- 定义 metadata、index、registration 和 artifact 的对象边界。
- 将协议错误映射为只读代理响应。

公共层负责 `SourcePool`、HTTP 请求安全、缓存存储、原子发布、完整性执行和
审计，不持有各生态的可选协议字段，也不通过一个万能代理处理未注册协议。

## 缓存、完整性与审计

### 文件系统存储

MVP 不使用数据库。配置在启动时从本地文件加载；metadata、artifact、Rustup
快照和审计事件使用本地文件系统。文件系统是当前缓存、快照和审计的唯一事实
存储，不存在隐藏的数据库索引或第二缓存事实源。

只有在需要跨实例协调、复杂审计查询、账号权限、发布审批或关系型制品治理时，
才重新评估数据库。仅仅为了缓存索引、来源回退或审计追加不引入数据库。

### 缓存键和来源

公共缓存逻辑键至少包含：

```text
ecosystem
entry_name
protocol_variant
object_key
representation
selected_proxy
```

`representation` 用于区分 PyPI HTML/JSON、不同 Accept 结果和其他协议表示；
`protocol_variant` 用于区分 Cargo sparse、NuGet registration/package content
等不同资源角色。entry、表示和来源必须隔离，不能只按包名或 URL 的最后一段
复用缓存。

所有成功内容按以下顺序发布：

```text
下载临时文件
  -> 解析或完整性校验
  -> 写入对象元数据和来源绑定
  -> 原子替换当前对象
```

缓存命中也必须检查当前对象的完整性信息。上游没有提供摘要时，可以计算并记录
本地摘要，但不得把本地计算结果伪装成上游声明的摘要。

公共 TTL 语义统一使用秒：正数表示该对象在 TTL 内可直接命中，`0` 表示不按
时间复用，`-1` 表示对象存在时不主动重新检查。具体生态可以区分 metadata 和
artifact，但不得重新解释这些值。旧缓存是否允许 stale serving 必须由适配器
契约明确，不能在实现中隐式决定。

### 错误边界

- 路径非法、路径穿越、未知入口或协议对象不存在：`404`。
- 客户端表示或 Accept 不支持：按协议返回 `406` 或等价的明确客户端错误。
- 来源明确拒绝的请求：直接返回对应上游错误，不无条件回退。
- 来源不可用、响应无法解析或完整性校验失败：允许 group 继续尝试下一个
  proxy；没有可用来源时返回 `502`。
- 已有可服务的旧缓存而刷新失败：仅在该生态契约允许时返回旧缓存，并记录
  stale 命中和失败原因。
- 本地缓存、原子写入或审计存储失败：返回 `500`，不能用上游回退掩盖本地
  数据损坏。

### 审计边界

审计至少记录 `ecosystem`、入口 `name`、协议对象键、`selected_proxy`、回退
链、metadata/artifact 类型、cache hit/miss、发布结果和失败类别。不记录上游
响应正文、访问凭据或完整制品内容。

## 生态适配器摘要

### Rustup

Rustup 保留 channel manifest 刷新、URL 重写、dist 路径白名单、SHA-256 校验、
快照和制品缓存。规范入口为 `/mirror/rustup/{name}/...`；旧
`/mirror/rustup/dist/**` 不再注册。

### Cargo

Cargo 当前实现 Sparse Index 和 crate 下载，规范入口为
`/mirror/cargo/{name}/...`。但当前 Cargo MVP 仍按请求独立通过 SourcePool 获取
index 和 crate，尚未实现上述通用的 metadata current/sidecar affinity，也没有
在 artifact 请求前读取并使用通用 `selected_proxy` 绑定。因此不能把公共 affinity
契约写成 Cargo 已完成能力；Cargo 后续修订必须补齐该绑定、摘要校验后的回退和
sidecar 更新。Cargo Git Index、publish 和写入操作仍不在范围内。详细契约见
`cargo-mvp.md`。

### npm 与 UPM

npm 和 UPM 都可以复用 npm-compatible metadata、版本对象和 tarball 下载的
协议核心，但它们是两个生态入口：

```text
/mirror/npm/{name}/...
/mirror/upm/{name}/...
```

UPM 另行处理 scoped registry 和 Unity package scope；不能通过 npm 入口或一个
全局 scope 分支伪装成已实现的 UPM 支持。详细契约见 `npm-upm-mvp.md`。

### PyPI

PyPI 使用 `/mirror/pypi/{name}/simple/...`，适配器负责项目名规范化、HTML/JSON
表示协商、文件链接重写和 hash 校验。详细契约见 `pypi-mvp.md`。

### NuGet

NuGet 使用 `/mirror/nuget/{name}/v3/...`，适配器负责 service index、registration
和 package content；搜索及写入型 API 保持后续独立范围。详细契约见
`nuget-mvp.md`。

### Docker

Docker 使用 `/mirror/docker/{name}/...`。适配器首期处理 Registry V2 的 manifest、
image config 和 layer blob，支持 tag、digest 和多架构 manifest。Docker 的内容
寻址模型要求按 digest 校验并缓存 blob；tag 到 manifest 的解析结果作为 metadata
处理。推送、删除、账号和签名管理不在只读代理范围内。

Docker source 的 `base_url` 是一个 Registry V2 根地址。具体的
`/v2/{repository}/manifests/{reference}`、`blobs/{digest}` 等协议路径由适配器
生成，不向公共 source 配置增加多个地址字段。

### VS Code 扩展

VS Code 扩展使用 `/mirror/vscode/{name}/...`。适配器负责扩展 metadata、版本
选择和 `.vsix` 文件下载；具体支持 Microsoft Marketplace、Open VSX 或内部
兼容源需要在实现任务中按实际上游协议固定，不能把某个平台 API 当作通用协议。
发布、删除、账号和 token 管理不在范围内。

### Winget

Winget 使用 `/mirror/winget/{name}/...`。适配器负责 source/index、package
manifest 和安装包下载，重写 manifest 中的下载地址，并校验
`InstallerSha256`。安装包来源必须经过配置来源或协议适配器的安全校验；发布、
manifest 写入、账号和签名管理不在范围内。

## 验证边界

每个适配器交付时必须拥有独立的本地 HTTP 协议测试，覆盖合法路径、非法路径、
metadata 解析、URL 重写、来源回退、缓存隔离和完整性失败。公共层测试覆盖
source graph、单一 `base_url`、禁止任意 URL、重定向、原子写入和审计字段。

本 Task 只形成后续适配器文档，不声称 npm、UPM、PyPI 或 NuGet 已实现，也不为
未注册生态提供假路由。真实客户端联调、外部 registry 行为和生产部署验证必须
在对应适配器实现后单独执行。

当前 Cargo 的 affinity 缺口不影响已经完成的 Rustup 绑定语义，但意味着 Cargo、
npm、UPM、PyPI 和 NuGet 的后续适配交付都必须分别证明两级文件系统绑定、来源
回退和摘要校验；不能仅凭公共接口存在或单独的缓存测试声称已完成。

## 官方协议依据

- [npm Registry API](https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md)：
  package metadata、版本对象和 tarball 字段。
- [Unity Manual: Scoped registries](https://docs.unity3d.com/Manual/upm-scoped.html)：
  UPM registry 与 package scope 配置。
- [PyPA Simple Repository API](https://packaging.python.org/en/latest/specifications/simple-repository-api/)：
  项目名规范化、HTML/JSON 表示、Accept 协商、文件链接和 hash 属性。
- [NuGet API overview](https://learn.microsoft.com/en-us/nuget/api/overview) 与
  [service index](https://learn.microsoft.com/en-us/nuget/api/service-index)：
  V3 资源发现、registration 和 package content。
