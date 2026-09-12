# 使用指南

本文面向使用 Stacktory 的开发人员。它只描述当前仓库已经实现并注册的能力；
未实现的生态会明确标注，不能因为配置模板中存在 source 就直接使用。

## 当前状态

| 生态 | 当前状态 | 客户端 |
| --- | --- | --- |
| Rustup | 已实现 | `rustup`、`rustc`、`cargo` 工具链安装 |
| Cargo | 已实现 | Cargo Sparse Registry、依赖下载和 `cargo install` |
| VS Code | 已实现 | 受控 VS Code 产品副本的扩展侧栏 |
| Docker | 未实现 | 暂不可用 |
| npm | 未实现 | 暂不可用 |
| Unity UPM | 未实现 | 暂不可用 |
| PyPI | 未实现 | 暂不可用 |
| NuGet | 未实现 | 暂不可用 |

当前服务只在启动时读取当前工作目录的 `config.toml`。Docker Compose 将
`deploy/app/config.toml` 挂载为 `/app/config.toml`，运行数据写入
`deploy/data/`。

## 公共配置模型

每个生态使用独立的 `sources`：

```toml
[[<ecosystem>.sources]]
name = "official"
type = "proxy"
base_url = "https://upstream.example"

[[<ecosystem>.sources]]
name = "group"
type = "group"
members = ["official"]
```

`proxy` 是一个固定上游，`group` 按 `members` 声明顺序尝试 proxy。第一个成功
响应的 proxy 被选中；来源失败、允许回退的 404 或协议允许的完整性失败才会继续
尝试下一个成员。明确的客户端错误不会被无条件转换成回退。

客户端 URL 中的 `{name}` 是 Stacktory 的入口名：

```text
https://stacktory.example.internal/mirror/{ecosystem}/{name}/...
```

可以把 `group` 换成某个直接 proxy，例如 `official`。metadata 通常按入口隔离，
二进制 artifact 按实际 selected proxy 归属；因此 group 和 direct proxy 在绑定同一
实际来源与上游对象时可以复用 artifact。

生产环境的 `root_url` 必须是客户端可访问的 HTTPS 地址。`data_dir` 是相对于服务
启动工作目录的路径，服务不会从环境变量猜测配置文件位置。

## 启动服务

### 本地开发

在仓库根目录准备 `config.toml`，然后执行：

```text
cargo run
```

健康检查：

```text
curl http://127.0.0.1:18761/healthz
```

本地如果使用 HTTPS 反向代理，`root_url` 必须填写 HTTPS 入口，而不是后端监听
地址。后端可以继续监听 `127.0.0.1:18761`。

### Docker Compose

从部署模板创建实际配置：

```text
Copy-Item deploy/app/config.example.toml deploy/app/config.toml
docker compose -f deploy/docker-compose.yml up -d
```

启动前必须修改：

- `root_url`：改为实际 HTTPS 服务地址。
- 各生态 source：只填写已经确认存在对应协议资源布局的上游根地址。
- `group.members`：按期望的主源和回退顺序填写。

查看服务日志：

```text
docker compose -f deploy/docker-compose.yml logs -f stacktory
```

## Rustup

### 安装 rustup

首次安装 `rustup` 时仍使用 Rust 官方安装程序。Stacktory 的
`RUSTUP_DIST_SERVER` 只负责 Rustup 工具链和组件分发，不负责首次安装程序本身。

安装完成后确认：

```text
rustup --version
rustup show
```

### 配置分发地址

PowerShell 当前窗口配置：

```powershell
$env:RUSTUP_DIST_SERVER = "https://stacktory.example.internal/mirror/rustup/group"
```

Windows 用户级永久配置：

```powershell
[Environment]::SetEnvironmentVariable(
  "RUSTUP_DIST_SERVER",
  "https://stacktory.example.internal/mirror/rustup/group",
  "User"
)
```

设置永久变量后需要重新打开终端。验证：

```powershell
Get-Item Env:RUSTUP_DIST_SERVER
```

### 安装和切换工具链

```text
rustup toolchain install stable
rustup default stable
rustup component add rustfmt clippy
rustup target add x86_64-pc-windows-msvc
```

指定 nightly：

```text
rustup toolchain install nightly
rustup override set nightly
```

`RUSTUP_DIST_SERVER` 是工具链分发根地址；如果 Stacktory 没有实现 Rustup 自更新
资源，不要把 `RUSTUP_UPDATE_ROOT` 盲目改成 Stacktory 地址。Rustup 官方文档将
`RUSTUP_DIST_SERVER` 定义为 Rust 静态资源下载根地址。

### 使用直接 proxy

将入口中的 `group` 换成配置中的 proxy 名称：

```powershell
$env:RUSTUP_DIST_SERVER = "https://stacktory.example.internal/mirror/rustup/official"
rustup toolchain install stable
```

Rustup 会先请求 channel manifest。manifest 中每个制品保存实际来源和 SHA-256；
后续下载按该制品绑定的来源获取，不因为入口是 group 就把所有制品放入 group
目录。

## Cargo

### 配置 crates.io 源替换

在用户 Cargo 配置 `%USERPROFILE%\.cargo\config.toml`，或项目的
`.cargo/config.toml` 中写入：

```toml
[source.crates-io]
replace-with = "stacktory"

[source.stacktory]
registry = "sparse+https://stacktory.example.internal/mirror/cargo/group/"
```

直接使用某个 proxy 时：

```toml
[source.crates-io]
replace-with = "stacktory"

[source.stacktory]
registry = "sparse+https://stacktory.example.internal/mirror/cargo/official/"
```

验证配置：

```text
cargo fetch -vv
cargo build
```

安装和使用 crate：

```text
cargo install ripgrep
cargo install --locked ripgrep
cargo test
```

Cargo 的 source replacement 适用于镜像与被替换源内容等价的场景。Stacktory 当前
只提供 Sparse Index 和 crate 下载，不提供 Cargo Git Index、publish、push 或
私有 registry 账号认证。

Cargo 缓存分为入口级 index 和实际来源级 crate：

```text
data/cargo/metadata/{entry}/index/
data/cargo/artifacts/{proxy}/objects/crates/
```

## VS Code 扩展

当前 VS Code 适配器对接 Microsoft Marketplace Gallery。它要求使用受控 VS Code
产品副本的产品级 `extensionsGallery` 配置；不能通过普通用户设置中的
`extensions.gallery.serviceUrl` 替换，因为该路径会进入 VS Code 私有 Marketplace
登录逻辑。

产品配置示例：

```json
{
  "extensionsGallery": {
    "serviceUrl": "https://stacktory.example.internal/mirror/vscode/group",
    "resourceUrlTemplate": "https://stacktory.example.internal/mirror/vscode/group/resource/{publisher}/{name}/{version}/{path}",
    "extensionUrlTemplate": "https://stacktory.example.internal/mirror/vscode/group/extension/{publisher}/{name}/latest"
  }
}
```

产品级 URL 必须使用受信任的 HTTPS。开发测试可以把本机测试证书导入当前用户的
受信任根证书存储，但不要修改日常 VS Code 安装。

打开扩展面板后，正常搜索、查看详情并点击安装即可。Stacktory 支持的主要路由：

```text
POST /mirror/vscode/{name}/extensionquery
GET  /mirror/vscode/{name}/asset/{asset-id}
GET  /mirror/vscode/{name}/asset-base/{asset-base-id}/{asset-type}
GET  /mirror/vscode/{name}/resource/{publisher}/{name}/{version}/{path}
GET  /mirror/vscode/{name}/extension/{publisher}/{name}/latest
```

排查安装是否经过 Stacktory：

```text
data/audit/events.jsonl
```

重点查看 `vscode_marketplace_query_published`、`vscode_marketplace_asset_published`
和 `vscode_marketplace_asset_read`。VS Code artifact 位于：

```text
data/vscode/artifacts/{proxy}/objects/marketplace/
```

## Docker

当前 Docker Registry adapter 尚未实现，也没有注册 `/mirror/docker` 路由。虽然
配置模板已经包含 Docker source，但现在不能把 Docker daemon 的
`registry-mirrors` 直接指向 Stacktory，也不能执行基于该入口的 `docker pull`。

完成 Docker Registry V2/OCI Distribution 只读适配器后，文档需要补充：

- Docker Registry V2 能力探测。
- tag、digest、manifest、image index、config 和 blob 的客户端配置。
- Docker daemon 或专用 registry hostname 的实际映射方式。
- digest 校验、group 回退和真实 `docker pull` 验证。

在 adapter 注册前不要把下面路径作为可用地址：

```text
/mirror/docker/group/...
```

## npm

当前 npm adapter 尚未实现，也没有注册 `/mirror/npm` 路由。暂时不要执行：

```text
npm config set registry https://stacktory.example.internal/mirror/npm/group
```

完成后，目标使用方式是把 npm registry 指向某个 Stacktory 入口，然后正常执行：

```text
npm install <package>
npm ci
```

实现必须先覆盖普通 package、scoped package、metadata 中 dist URL 重写、integrity
或 shasum 校验、tarball 缓存和真实 npm CLI 联调。

## Unity UPM

当前 UPM adapter 尚未实现，也没有注册 `/mirror/upm` 路由。Unity Package Manager
暂时不能使用 Stacktory source。

完成后目标使用方式是在项目 `Packages/manifest.json` 中配置 scoped registry：

```json
{
  "scopedRegistries": [
    {
      "name": "Stacktory",
      "url": "https://stacktory.example.internal/mirror/upm/group",
      "scopes": [
        "com.example"
      ]
    }
  ]
}
```

该示例只有在 UPM scope 配置、npm-compatible metadata、tarball 路由和 Unity 客户端
联调完成后才可使用。UPM 必须保持独立的 `/mirror/upm` 入口，不能伪装成 npm。

## PyPI

当前 PyPI adapter 尚未实现，也没有注册 `/mirror/pypi` 路由。暂时不要把 pip
配置到 Stacktory。

完成后目标配置为：

```text
python -m pip config set global.index-url https://stacktory.example.internal/mirror/pypi/group/simple/
python -m pip install <package>
```

实现必须覆盖项目名规范化、Simple API HTML/JSON 表示协商、文件链接重写、wheel 和
sdist hash 校验、group 回退以及真实 pip 安装。未实现前，上述命令不能执行。

## NuGet

当前 NuGet adapter 尚未实现，也没有注册 `/mirror/nuget` 路由。暂时不要把
Stacktory 添加为 NuGet source。

完成后目标使用方式为：

```text
dotnet nuget add source https://stacktory.example.internal/mirror/nuget/group/v3/index.json --name stacktory
dotnet restore --source stacktory
```

实现必须覆盖 V3 service index、registration、package content、selected_proxy
绑定、nupkg 校验、group 回退和真实 `dotnet restore`。NuGet 搜索、push、delete、
账号和 API key 不属于首期只读范围。

## 缓存与审计

缓存目录是服务数据，不是客户端配置目录。当前结构：

```text
data/
├─ rustup/
├─ cargo/
├─ vscode/
└─ audit/events.jsonl
```

查看来源和缓存状态时优先看 JSONL 审计，而不是根据文件名猜测来源。artifact 文件
使用内容或协议对象键命名，不保证带有原始包名和版本名。

清理缓存前应停止服务，并只处理确认过的运行数据目录。当前 MVP 尚未提供容量、
TTL、审计轮转或自动淘汰命令，不能把手工删除当成长期运维方案。

## 相关文档

- [Rustup 适配器契约](architecture/rustup-mvp.md)
- [Cargo 适配器契约](architecture/cargo-mvp.md)
- [VS Code Marketplace 适配器契约](architecture/vscode-mvp.md)
- [多生态代理总设计](architecture/multi-ecosystem-readonly-proxy.md)
- [部署配置模板](../deploy/app/config.example.toml)
