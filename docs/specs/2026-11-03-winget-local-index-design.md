# Winget 本地索引设计

## 目标

让完全内网的 Windows 客户端只访问 Stacktory，就能完成 Winget 搜索、manifest
读取和安装包下载。只有 Stacktory 所在服务器需要访问互联网。

最终形态不引入 WinGetty 或第二层 Winget 源服务。Stacktory 自己维护 Winget
目录索引、manifest 镜像和安装包缓存。

## 非目标

- 不代理微软默认的 `Microsoft.PreIndexed.Package` 协议。
- 不实现 Winget 写入、发布、删除或私有认证。
- 不把安装包全量预下载；安装包按客户端实际请求缓存。
- 不改变现有 REST proxy 模式的来源回退语义。
- 不改变客户端可见的 `/mirror/winget/{name}/...` 路径。

## 背景与约束

微软默认 `winget` 源是：

```text
https://cdn.winget.microsoft.com/cache
Microsoft.PreIndexed.Package
```

它提供 `source.msix` 预索引包，不提供 `/information`、
`/manifestSearch` 和 `/packageManifests`。当前 Stacktory 的 Winget 适配器只支持
`Microsoft.Rest`，不能直接使用这个默认源。

`source.msix` 当前约 22.5 MB，解压出的 `Public/index.db` 约 45.6 MB，包含约
19 万条 manifest 记录和 6.9 万个版本。它是目录索引，不是完整 manifest 内容。

`microsoft/winget-pkgs` 仓库当前约 0.9 GB，包含 manifest YAML，不包含安装包。
全量安装包不适合预下载，客户端请求什么就缓存什么。

## 总体架构

新增 Winget `index` source。它和现有 `proxy`、`group` 一样是来源模型的一部分，
但由 Stacktory 本地目录提供数据，不调用上游 REST 服务。

```text
内网 Winget 客户端
  -> Stacktory /mirror/winget/{local}/...
  -> 本地 source.msix 目录索引 + 本地 winget-pkgs manifest
  -> 按需访问公网安装包地址并缓存
```

现有 `proxy` source 和 `group` source 保持不变，用于有真实 Microsoft.Rest
上游的场景。`index` source 是独立的直接入口，不加入 group。

## 配置

Winget 只新增一个刷新间隔，和 Rustup 的刷新间隔处于同一层级：

```toml
[winget]
refresh_interval_secs = 86400

[[winget.sources]]
name = "local"
type = "index"
base_url = "https://cdn.winget.microsoft.com/cache"
```

约束：

- `index` source 的 `base_url` 必须是 HTTPS，适配器请求
  `<base_url>/source.msix`。
- `index` source 不接受 `installer_hosts`；它使用下面的公网主机策略。
- 现有 `proxy` source 继续要求 `installer_hosts`，`group` 继续不能配置
  `installer_hosts`。
- `refresh_interval_secs` 缺省 86400 秒，必须大于零。
- 不新增索引、manifest、安装包的独立保留参数；分别复用
  `[retention].metadata_retention_secs`、`[retention].artifact_idle_secs` 和
  `[retention].binding_idle_secs`。

## 数据布局

索引和 manifest 都放在现有 `stores/winget` 下，使用 revision 目录和
`current.toml` 指针：

```text
<data_dir>/stores/winget/catalog/<revision>/source.msix
<data_dir>/stores/winget/catalog/<revision>/index.db
<data_dir>/stores/winget/catalog/current.toml
<data_dir>/stores/winget/manifests/repository.git
<data_dir>/stores/winget/manifests/<revision>/...
<data_dir>/stores/winget/manifests/current.toml
<data_dir>/stores/winget/artifacts/...                 现有安装包缓存
<data_dir>/stores/winget/metadata/...                 现有 binding sidecar
```

`current.toml` 至少记录 revision、来源 URL、发布时间、文件摘要和 manifest
commit。当前 revision 永远保留；上一 revision 作为同步失败时的回退。更旧的
revision 交给统一 metadata retention 清理。

## 同步与发布

服务启动后立即执行一次同步，之后按 `refresh_interval_secs` 执行。同步是后台
任务，不阻塞其他生态的启动。

目录索引同步：

1. 通过现有 HTTP 客户端下载 `<base_url>/source.msix`，使用 `[proxy]` 和
   `[upstream].metadata_timeout_secs`。
2. 下载到临时目录，校验 ZIP 结构、`Public/index.db` 存在、SQLite 文件头及
   必要表和列。
3. 解压到新的 `catalog/<revision>`。
4. 原子写入新的 `catalog/current.toml`。

manifest 同步：

1. 在 `manifests/repository.git` 维护一个只读裸仓库，使用
   `git clone --mirror` 初始化，之后 `git fetch` 增量更新。
2. 通过 `git archive` 把当前 commit 导出到新的
   `manifests/<revision>`，不直接在正在服务的目录里改写文件。
3. 原子写入新的 `manifests/current.toml`。

Git 子进程必须：

- 使用 HTTPS；
- 解析 `[proxy]` 并设置 `HTTP_PROXY`/`HTTPS_PROXY`，遵守 `no_proxy`；
- 设置 `GIT_TERMINAL_PROMPT=0`，禁止交互式凭据；
- 失败时保留旧 revision，不把半成品发布给客户端。

同步失败时继续提供旧 revision，并记录 `winget_index_sync_failed`。没有可用
revision 时，Winget 元数据入口返回 `503`，不能用不完整索引提供结果。

## 查询与响应

`/information` 在索引就绪后返回固定入口标识和
`ServerSupportedVersions = ["1.0.0"]`；未就绪时返回 `503`。

`/manifestSearch` 查询 `index.db` 的包、名称、版本、标签和标识表，按
Winget REST 1.0 的 `Query`、`Filters`、`Inclusions` 和 `MaximumResults`
返回包列表。支持索引中已有的 PackageIdentifier、PackageName、Moniker、Tag、
Command、ProductCode、UpgradeCode、NormalizedName、NormalizedPublisher 和
Version 条件；无法映射的过滤条件返回 `400`，不静默忽略。

`/packageManifests/{id}` 使用 index.db 的 `pathparts` 关系定位版本目录，再从
当前 manifest revision 读取该目录下的 YAML：

- 将版本、installer 和 locale manifest 合并为 Winget REST 的 `Data.Versions`
  结构；
- 保留未知字段；
- 带 `Version` 时只校验并返回目标版本；
- 版本不存在返回 `404`；
- manifest 解析失败按来源失败返回 `502`，不返回部分 manifest；
- 继续使用现有的安装包 URL 重写、token、binding sidecar 和完整性校验。

index.db 的 schema 是外部输入。同步时校验必需表/列；schema 不匹配时拒绝发布
新 revision 并保留旧 revision，不把数据库结构变化误报成包不存在。

## 安装包下载与安全

`index` 模式不再维护人工 `installer_hosts`，因为完整 Winget 的安装包主机集合
很大且持续变化。安全策略为：

- 只允许 `https`；
- URL 不含凭据、fragment、反斜杠、控制字符和 `.`/`..` 路径段；
- DNS 解析后的所有地址必须是公网地址，拒绝 loopback、私网、链路本地和保留
  地址；
- 每一跳重定向重新执行相同校验，最多三跳；
- 只允许从微软预索引和本地 manifest 得到的安装包 URL，不接受客户端任意 URL；
- 安装包仍先校验 SHA-256，再发布到 artifact cache。

现有 `proxy` 模式继续使用显式 `installer_hosts` 白名单，不放宽它的边界。

下载使用现有 `[proxy]`、`[upstream].artifact_timeout_secs`、
`FetchOperation::Artifact`、artifact budget 和审计，不建立第二套 HTTP 客户端。

## 生命周期与审计

- 目录索引和 manifest revision 属于 metadata，使用
  `metadata_retention_secs` 清理旧 revision；当前和上一 revision 固定保留。
- 安装包对象使用 `artifact_idle_secs` 淘汰。
- installer binding 使用 `binding_idle_secs` 淘汰。
- `cleanup_interval_secs` 继续控制后台清理周期。
- 同步、发布、读取、搜索、manifest、安装包和回退都写入现有 JSONL 审计。
- 不新增数据库服务；`index.db` 是只读索引文件，不是运行时数据库。

## 依赖与运行环境

实现需要：

- 只读 SQLite 访问能力，用于 `index.db`；
- ZIP 解压能力，用于 `source.msix`；
- YAML 解析能力，用于 `winget-pkgs` manifest；
- 运行镜像内提供 `git`，供 manifest 镜像同步使用。

新增依赖必须使用当前 Rust 1.98 工具链可构建的稳定版本，并纳入
`cargo build --release --locked` 门禁。

## 验证

测试不下载 0.9 GB 的真实仓库，使用最小 fixture：

- 伪造 source.msix 和 index.db，验证同步、schema 校验和原子发布；
- 伪造本地 manifest revision，验证搜索、版本过滤和 YAML 合并；
- 验证首次同步前返回 503，同步失败后继续使用旧 revision；
- 验证安装包公网策略拒绝私网、重定向回环、HTTP 和非 HTTPS；
- 验证安装包 SHA-256、缓存命中、binding 和 artifact retention；
- 验证现有 REST proxy 和 group 行为不回归；
- 全量运行 `cargo fmt --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
  `cargo test --all-targets --all-features`、`cargo build --release --locked`。

## 分阶段交付

1. 配置 source 类型、revision 布局、readiness 和文档。
2. source.msix 下载、解压、SQLite 校验与目录索引发布。
3. `/information`、`/manifestSearch` 和索引查询。
4. `winget-pkgs` 镜像同步、manifest 选择、YAML 合并和版本过滤。
5. 安装包公网策略、现有重写/缓存/binding 接入。
6. 统一 retention、审计、失败回退和全量验证。

## 成功标准

- 完全内网客户端可以添加 Stacktory 的 `Microsoft.Rest` source；
- 能搜索全量 Winget 包并读取指定版本 manifest；
- 能安装至少一个真实包，客户端不访问除 Stacktory 外的任何网络地址；
- 同步失败不会破坏上一个可用 revision；
- 目录索引、manifest 和安装包分别遵守统一 metadata/artifact/binding 生命周期；
- 不再需要 WinGetty，也不再需要维护人工 Winget 安装包主机列表。
