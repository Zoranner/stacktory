# Stacktory 多生态只读代理架构设计

## 目标

Stacktory 为内网开发机提供统一的研发制品访问入口。当前实现包含 Rustup
动态镜像、Cargo Sparse Index/crate、VS Code Marketplace 扩展、npm、NuGet V3
restore、Docker Registry V2 和 Unity UPM 只读代理；PyPI 和 Winget
只在本文件及各自 MVP 文档中定义适配边界，未注册的生态不暴露路由。

本设计约束公共来源访问、显式组合、缓存、完整性和审计职责，使新增生态只
实现协议适配器，不复制公共行为。本设计不引入数据库，不提供写入型包管理
能力，也不接受客户端传入任意上游 URL。

## 范围

### 已实现能力

- Rustup 命名入口和动态 manifest 镜像。
- Cargo Sparse Index 与 crate 下载。
- VS Code Marketplace 扩展 metadata 与 `.vsix` 代理。
- npm 与 Unity UPM 的 npm-compatible metadata 与 tarball 代理，UPM 使用独立
  生态入口和 scope 选择规则。
- NuGet V3 只读 restore、只读查询和漏洞数据。
- Docker Registry V2 只读拉取、只读查询和 CLI 根入口。
- 公共 `sources` 配置、`proxy`/`group` source graph、来源回退、文件缓存、
  原子写入、完整性校验和 JSONL 审计。
- 统一路由模型：

  ```text
  /mirror/{ecosystem}/{name}/...
  ```

### 未实现的适配边界

以下生态只有适配边界文档，没有注册路由，本文不描述其运行时行为：

- PyPI：Simple API 的 HTML/JSON 表示和项目文件下载。
- Winget：source/index、package manifest 和安装包下载。

### 明确不在范围内

- npm publish、unpublish、账号和 token 管理。
- PyPI 上传、删除、账号和 token 管理。
- NuGet push、delete、API key、账号和动态注册服务。
- Docker 镜像推送、删除、账号、签名管理和构建服务。
- VS Code 扩展发布、删除、账号和 token 管理。
- Winget source 发布、manifest 写入、账号和签名管理。
- UPM 发布、包写入、Unity Editor、Asset Store 和 Git URL 包。
- 管理后台、动态修改配置、远程配置中心和数据库。
- 任意 URL 代理、客户端指定上游、未验证的跨来源重定向。

## 配置分层

配置文件按职责分为四层，每层只有一个含义：

| 层级 | 配置节 | 职责 |
| --- | --- | --- |
| 进程端点 | `[server]` | 绑定地址、绑定端口和客户端可见的 `public_url` |
| 存储路径 | `[storage]` | 数据目录和运行日志目录 |
| 横切策略 | `[proxy]`、`[retention]` | 出站代理和保留清理 |
| 生态来源 | `[rustup]`、`[cargo]`、`[docker]`、`[npm]`、`[nuget]`、`[upm]`、`[vscode]` | 各生态的 `sources` source graph |

`server.address`、`server.port` 只负责进程监听，`server.public_url` 负责生成
客户端可见地址，两者可以因反向代理而不同。`public_url` 可写单个字符串或数组；
数组第一项是默认地址，请求按 `Host` 与 `X-Forwarded-Proto` 精确匹配数组项，
匹配不到时使用第一项，不会反射任意请求 Host。反向代理需要转发 `Host` 和
`X-Forwarded-Proto`，否则同一域名的 HTTP 与 HTTPS 入口无法区分。`public_url`
可包含路径前缀，但不能包含凭据、query 或 fragment；显式配置的 HTTP 地址允许
在非 localhost 上使用。
所有配置节都拒绝未知字段，旧的扁平键、旧字段名和未实现的生态配置不会被
静默接受。

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
allow_http = false
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
通过 scheme、host、凭据、query、fragment、路径和 `allow_http` 校验。适配器
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

数据目录只分两类：`stores` 下按生态分片的数据，以及 `audit` 下的全局审计事实。
适配器只负责在 `metadata` 或对象根之下拼接协议对象键，路径前缀由
`src/storage/layout.rs` 统一生成，不在各适配器重复拼接。

```text
<data_dir>/stores/<ecosystem>/metadata/...                  入口拥有的元数据与来源绑定
<data_dir>/stores/<ecosystem>/artifacts/<来源>/objects/...  实际来源拥有的对象
<data_dir>/stores/rustup/snapshots/<入口>/<channel>/...     Rustup 快照
<data_dir>/audit/events.jsonl                               全局审计事件
<log_dir>/stacktory.log                                     运行日志（默认 `logs`）
```

运行日志不属于数据目录：它由 `log_dir` 指定，服务启动时创建并追加写入，同时
保留 stderr 输出，便于容器和本地调试各自取用。

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

Rustup manifest/artifact 和 Docker blob 的单飞（single-flight）锁按对象键哈希到
固定数量的分片，不按访问过的对象数量创建新锁。不同对象可能共享分片并短暂
串行，但内存上界固定；同一对象的并发请求仍由同一分片互斥，避免重复上游请求。

所有生态和 Rustup 的 artifact 下载共享同一个固定并发预算，默认同时最多 4 个
上游制品请求。预算只约束制品传输，metadata 请求不占用该预算；单个生态不能再
通过增加 `SourcePool` 扩大全局并发。

`metadata_retention_secs` 是 metadata 文件的清理窗口，不是请求路径的缓存 TTL；配置
值必须为正整数，当前没有 `0` 或 `-1` 的特殊语义。请求路径的重新验证窗口由
各适配器契约固定，例如 npm/UPM、Cargo 和 NuGet 当前都使用 15 分钟。旧缓存是否
允许 stale serving 必须由适配器契约明确，不能在实现中隐式决定。

### 错误边界

- 路径非法、路径穿越、未知入口或协议对象不存在：`404`。
- 客户端表示或 Accept 不支持：按协议返回 `406` 或等价的明确客户端错误。
- 所有生态的 group 对 `401`、`403`、`404`、`408`、`429`、`5xx` 和网络失败
  继续尝试下一个来源。`401`/`403` 表示来源级访问限制，`404` 表示该来源缺失
  对象，二者都不能证明下一个来源同样失败。
- 其他 `4xx` 属于请求语义或协议协商错误，直接返回，不在来源之间重试。
- 直接 source 入口不向兄弟来源回退，仍直接返回该来源的上游错误。
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

### 数据生命周期

metadata 和审计文件按配置的保留窗口清理，配置由 `[retention]` 承载：

```toml
[retention]
metadata_retention_secs = 604800
audit_retention_days = 30
cleanup_interval_secs = 3600
rustup_snapshot_revisions = 3
artifact_idle_secs = 7776000
log_retention_days = 30
```

- 服务启动后立即执行一次清理，之后按 `cleanup_interval_secs` 周期执行。
- `<data_dir>/stores/<ecosystem>/metadata/**` 中修改时间早于 `metadata_retention_secs` 的
  文件会被删除，客户端下次请求时重新获取并重建缓存与来源绑定。该值是 metadata
  的保留上限，是否按时间复用缓存仍由各适配器的命中策略决定。
- `<data_dir>/audit/events.jsonl` 跨天后轮转为 `events.<UTC 日期>.jsonl`，超过
  `audit_retention_days` 的轮转文件被删除。
- `<log_dir>/stacktory.log` 按同样的规则轮转为 `stacktory.<UTC 日期>.log`，超过
  `log_retention_days` 的轮转文件被删除。
- Rustup 每个 `{entry}/{channel}` 只保留最近 `rustup_snapshot_revisions` 个
  修订；`current.toml` 指向的修订始终保留，指针存在但无法解析时跳过该 channel。
- 制品对象按闲置时间淘汰：`<data_dir>/stores/<ecosystem>/artifacts/**` 中修改时间
  早于 `artifact_idle_secs`（缺省 90 天）的文件会被删除，`artifact_idle_secs = 0`
  关闭该清理。制品旁的 sidecar 在命中缓存时与主对象一起更新访问时间，两者共享
  同一个淘汰窗口。
  命中缓存时适配器通过 `note_access` 更新对象修改时间，因此"修改时间"表示最近
  一次访问时间，而不是写入时间。制品都可以重新获取，删除不会造成不可恢复的
  数据丢失；不实现容量上限，淘汰只由闲置时间决定。
- 清理按轮次记录 `metadata_cleanup`、`artifact_cleanup`、`snapshot_cleanup`、
  `audit_cleanup` 和 `log_cleanup` 事件，包含删除数量；维护命令记录
  `legacy_layout_cleanup`。不逐文件写审计，也不记录文件内容。

旧布局留下的缓存不随服务启动或周期清理删除，只能显式执行维护命令：

```text
stacktory prune-legacy-layout --dry-run
stacktory prune-legacy-layout
```

命令删除两类内容：平铺布局时期的 `<data_dir>/<ecosystem>/` 整个目录，以及
`<data_dir>/stores/<ecosystem>/artifacts/<来源>/` 下 `objects` 之外的条目。
只有同时包含 `metadata`、`artifacts` 或 `snapshots` 子目录的顶层目录才会被当作
生态目录删除，`stores` 和 `audit` 始终保留。`--dry-run` 只统计不删除。

## 生态适配器摘要

### Rustup

Rustup 保留 channel manifest 刷新、URL 重写、dist 路径白名单、SHA-256 校验、
快照和制品缓存。规范入口为 `/mirror/rustup/{name}/...`；旧
`/mirror/rustup/dist/**` 不再注册。

### Cargo

Cargo 当前实现 Sparse Index 和 crate 下载，规范入口为
`/mirror/cargo/{name}/...`，并已按上述通用 affinity 契约绑定来源：index 成功响应
持久化 `selected_proxy` sidecar；crate 请求先使用该绑定，只在绑定来源不可用、
响应无法使用或 checksum 不匹配时才按 group 顺序回退；回退得到的 crate 通过
checksum 校验后才原子发布 artifact 并更新 sidecar。index 与 crate 仍分别由
SourcePool 获取，不合并不同来源的版本记录。Cargo Git Index、publish 和写入操作
仍不在范围内。详细契约见 `cargo-mvp.md`。

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

PyPI 尚未注册路由。目标契约使用 `/mirror/pypi/{name}/simple/...`，适配器负责
项目名规范化、HTML/JSON 表示协商、文件链接重写和 hash 校验。详细契约见
`pypi-mvp.md`。

### NuGet

NuGet 已注册 `/mirror/nuget/{name}/v3/...` 及 Search、漏洞数据只读入口，适配器
负责 service index、Search、registration、package content 和漏洞数据的资源
发现、关系校验、来源绑定、摘要校验和 group 回退；写入型 API 保持后续独立范围。
详细契约见 `nuget-mvp.md`。

### Docker

Docker 使用 `/mirror/docker/{name}/v2/...`。适配器处理 Registry V2 的 `/v2/`
探测、manifest、manifest list、OCI image index、image config 和 layer blob，
支持 tag、digest、`Accept`、`Content-Type`、`Docker-Content-Digest` 和 blob
`Range` 语义。Docker 的内容寻址模型要求按 digest 校验并缓存对象；tag 到
manifest 的解析结果作为 entry-scoped metadata 处理。

只读查询还提供 Registry V2 的 `_catalog`、`{repository}/tags/list`，以及
Stacktory 统一的 `/mirror/docker/{name}/search?q=...` 仓库名搜索。搜索只在一个
selected proxy 的 catalog 分页结果内执行，不合并不同来源，也不实现 Docker
Registry V1 搜索。

Docker source 的 `base_url` 是一个 Registry V2 根地址。Stacktory 对外的
`/mirror/docker/{name}/v2/...` 与上游的 `/v2/...` 是两层不同路径；具体的
`/v2/{repository}/manifests/{reference}`、`/v2/{repository}/blobs/{digest}`
等协议路径由适配器生成，不增加 `/mirror/docker/v2/...` 内部命名空间，也不向
Docker 客户端返回上游 Registry host。

为兼容标准 Docker Registry 客户端，Docker source 可以成对配置可选的
`listen_port` 和 `public_url`。Stacktory 为每个配置端口启动
独立 listener，并将该 listener 的根 `/v2/...` 固定绑定到对应 source；客户端
可见地址来自 `public_url`。两个字段必须同时存在，多个 source 的端口和
公开地址必须唯一，端口不能占用 `server.port`。公开地址不能包含路径、查询参数
或片段，域名和 TLS 终止由外部 DNS、端口映射或反向代理完成。未配置这对字段的
source 不增加根 Registry 入口。
Docker 入口的 `public_url` 保持单值，它是 Registry 客户端地址，不参与主站
`server.public_url` 的多地址匹配。
Docker listener 与主监听共享关闭信号；主监听退出后，服务会等待 Docker listener
完成在途请求再结束进程。

manifest、image config 和 layer blob 分别记录 object type。group 只按声明顺序
选择一个通过协议和完整性校验的 proxy，成功后记录 `selected_proxy`，不合并
不同来源的 manifest 或 tag 集合。entry metadata 保持入口隔离；已校验内容按
实际 `selected_proxy`、repository、object type 和 digest 复用，因此 group
入口与其实际选中的 direct proxy 可以复用同一内容对象。

Docker manifest/tag 和 catalog/tags query metadata 记录来源指纹，由 proxy 名称、
`base_url` 和 `allow_http` 组成。同名 proxy 修改上游目标后，旧的 tag 映射和
查询结果按未命中重新获取；digest 校验通过的内容对象仍可按内容地址复用。

Docker 适配器首期不实现 push、delete、账号管理、私有凭据、签名管理、构建服务
和 Docker Registry V1 查询；对公开 Registry 返回的匿名 Bearer challenge，
Stacktory 在服务端获取短期 pull token，不向客户端暴露上游认证信息。真实
Docker/OCI 客户端联调必须在客户端支持当前 Stacktory 路径入口后单独验收。

### VS Code 扩展

VS Code 扩展使用 `/mirror/vscode/{name}/...`。适配器代理公开的 Microsoft
Marketplace Gallery，负责扩展 metadata、版本选择和 `.vsix` 文件下载；Open VSX
`/api/...` 协议属于后续独立适配，不能把某个平台 API 当作通用协议。发布、删除、
账号和 token 管理不在范围内。详细契约见 `vscode-mvp.md`。

### Winget

Winget 尚未注册路由。目标契约使用 `/mirror/winget/{name}/...`，适配器负责
source/index、package manifest 和安装包下载，重写 manifest 中的下载地址，并
校验 `InstallerSha256`。安装包来源必须经过配置来源或协议适配器的安全校验；
发布、manifest 写入、账号和签名管理不在范围内。

## 验证边界

每个适配器交付时必须拥有独立的本地 HTTP 协议测试，覆盖合法路径、非法路径、
metadata 解析、URL 重写、来源回退、缓存隔离和完整性失败。公共层测试覆盖
source graph、单一 `base_url`、禁止任意 URL、重定向、原子写入和审计字段。

真实客户端联调、外部 registry 行为和生产部署验证仍必须在对应适配器完成后单独执行；
本地协议测试通过不等于外部 `dotnet restore` 已验证。

Cargo、npm、UPM 和 NuGet 的来源绑定、回退和摘要校验仍分别受各自实现与测试约束；
不能仅凭公共接口存在或单独的缓存测试声称已完成。

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
