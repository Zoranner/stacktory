# Stacktory

Stacktory 是面向公司内网开发机的研发制品与动态工具链供应平台。服务器按照固定、
配置化的上游顺序获取内容，开发机只访问 Stacktory 的内网地址；客户端不能传入任意
上游 URL。

当前 MVP 实现 Rustup 动态工具链镜像适配器和 Cargo Sparse Index/crate 只读代理，
并提供 Rustup、Cargo 的命名来源配置模型。其他包与制品生态会继续位于统一的
`/mirror/{ecosystem}` 命名空间，尚未实现的能力标记为后续工作。

## Rustup 路由

| 路由 | 行为 |
| --- | --- |
| `GET /healthz` | 服务存活检查。 |
| `GET /mirror/rustup/{name}/dist/channel-rust-{channel}.toml` | 返回指定命名入口的内部化 channel manifest。 |
| `GET /mirror/rustup/{name}/dist/channel-rust-{channel}.toml.sha256` | 返回同一快照 manifest 的 SHA-256。 |
| `GET /mirror/rustup/{name}/dist/{path}` | 返回指定命名入口已发布 manifest 引用且校验通过的 Rustup 制品。 |

channel manifest 缺失或过期时由请求触发刷新。同一 channel 的并发刷新会合并；刷新
失败但已有旧快照时继续返回旧快照。制品按 manifest 中的 SHA-256 校验后原子写入
`data_dir/rustup/artifacts/{proxy}/objects/`。group 只决定 manifest 与制品的来源
选择，不参与二进制缓存键；同一个 proxy 的制品可被该 proxy 和引用它的 group 复用。
未知或非法路径不会触发上游请求。

Rustup 路由无需认证。运行审计追加到 `data_dir/audit/events.jsonl`，记录 manifest
发布、制品缓存和上游失败，但不记录上游响应正文。

## 部署

服务固定读取当前工作目录的 `config.toml`。仓库根目录的 `config.toml` 是本地开发默认
配置；Docker Compose 的工作目录为 `/app`，并将 `deploy/app/config.toml` 挂载为
`/app/config.toml`。运行数据为 `deploy/data/`，部署配置和运行数据均不会提交。
部署前先从 `deploy/app/config.example.toml` 创建实际配置，随后在 `deploy/` 下启动
Compose。

```text
docker compose up -d
```

生产配置必须使用 HTTPS 的 `root_url`。各 Rustup 上游默认要求 HTTPS；仅在
明确需要时，对对应条目设置 `require_https = false`。

内网开发机将 Rustup 分发服务器设置为：

```text
RUSTUP_DIST_SERVER=https://stacktory.example.internal/mirror/rustup/group
```

配置中的每个生态都使用命名 `sources`。`type = "proxy"` 声明一个上游地址，
`type = "group"` 按 `members` 顺序组合已声明的 proxy；未配置的生态保持禁用。
manifest 中的制品 URL 使用当前请求的 `/mirror/rustup/{name}` 根地址，并将 channel
作为 artifact URL 的绑定路径段。snapshot 为每个制品保存 SHA-256 和实际 `proxy`：
manifest 从当前 entry 的成员获取，但其中的制品 URL 只能匹配同一 Rustup 配置内已声明
的 proxy。artifact 请求按该制品保存的 proxy 获取，group 不参与 artifact 缓存键。

## Cargo 路由

| 路由 | 行为 |
| --- | --- |
| `GET /mirror/cargo/{name}/config.json` | 返回当前命名入口的 Sparse Registry 配置。 |
| `GET /mirror/cargo/{name}/{sparse-index-path}` | 返回校验过的 Sparse Index JSON Lines。 |
| `GET /mirror/cargo/{name}/crates/{crate}/{version}.crate` | 返回 index 中声明的 crate 制品。 |

Cargo crate 名称按 Cargo Sparse Index 标准规范化和分层。index 和 crate 下载都通过
对应的 `proxy` 或显式 `group` 访问；group 按成员顺序回退，404 缺失可回退，
401/403/408/429 等 terminal 状态保留。index 记录中的 `cksum` 是 crate 下载的
SHA-256 期望值，校验失败不会写入缓存，并会继续尝试 group 的下一个 proxy。

Cargo 缓存只使用文件系统：`data_dir/cargo/metadata/{name}/index/` 保存入口级 index，
`data_dir/cargo/artifacts/{proxy}/objects/crates/` 保存按实际来源隔离的 crate。group
只决定来源选择，不参与 crate 缓存键。客户端拿到的 `dl` 只指向 Stacktory 当前命名入口，
不暴露 Cargo 上游地址。

Cargo source 示例：

```toml
[cargo]

[[cargo.sources]]
name = "crates-io"
type = "proxy"
base_url = "https://pkg.example.internal/repository/cargo"

[[cargo.sources]]
name = "group"
type = "group"
members = ["crates-io"]
```

Cargo proxy 的 `base_url` 只能配置一个上游 registry 根地址。该根地址必须实际提供
`config.json` 和标准 Sparse Index 相对路径；crate 下载地址由该 proxy 的
`config.json` 中 `dl` 模板描述，可以指向经过安全校验的 HTTP/HTTPS 下载地址。
同 scheme/host/port 且位于该 proxy 根路径下的私有地址允许使用；跨 host 只允许
公共目标。私有或 Nexus proxy 不能只填写管理页面地址；应确认其实际代理资源布局
包含 Cargo registry 配置和 Sparse Index，并确认 `dl` 模板可被 Stacktory 安全解析。
Cargo 读取、缓存命中/未命中、回退、发布和失败会写入现有审计日志。

首期只实现 Sparse Index 和 crate 下载，不实现 Cargo Git Index、publish、push 或
其他生态协议。

## VS Code 扩展

VS Code 扩展面板通过 Microsoft Marketplace Gallery 协议访问：

| 路由 | 行为 |
| --- | --- |
| `POST /mirror/vscode/{name}/extensionquery` | 查询扩展、版本与资源。 |
| `GET /mirror/vscode/{name}/asset/{asset-id}` | 返回由已发布查询绑定的 Gallery 资源。 |

查询结果中的图标、详情、VSIX 等资源 URL 都改写为 Stacktory asset 路由。asset ID 只由
已发布的查询结果生成，客户端不能提交任意外部 URL。VSIX、图标和详情资源按实际 proxy
缓存到 `data_dir/vscode/artifacts/{proxy}/objects/marketplace/`。

VS Code 侧栏接入使用产品级 `extensionsGallery.serviceUrl`，不能使用用户 settings 的
`extensions.gallery.serviceUrl` 覆盖；后者会触发 VS Code 的私有 Marketplace 登录
流程。首期只支持公开 Microsoft Marketplace 的只读查询与资源下载，不支持发布、账号、
私有 Marketplace 或旧 Open VSX `/api/...` 协议。

## 发布

`vX.Y.Z` 标签会触发 GitHub Actions。工作流要求标签版本与 `Cargo.toml` 一致，执行
格式化、Clippy、测试和锁定依赖的 release 构建，然后发布 amd64/arm64 GHCR 镜像及带
SHA-256 文件的离线镜像包。定义工作流本身不会创建标签、镜像或 Release。
