# Stacktory

Stacktory 是自部署的内网只读下载服务，覆盖软件包、工具链和容器镜像。它把 Rust、Node.js、Python、.NET、Docker、Unity、VS Code 和 Windows 软件常用的上游源统一代理到内网，开发机只需要配置标准客户端，不用直连外网制品库。

Stacktory 只做只读下载，不提供发布、上传、删除、账号和权限管理；服务地址和入口名由 `config.toml` 决定。

## 特性

- **多生态统一入口**：Rustup、Cargo、npm/Bun、PyPI、NuGet、Docker、Unity UPM、VS Code 扩展和 Winget 由同一服务提供，不需要分别部署多套镜像。
- **Rust 工具链镜像**：Rustup 的 channel manifest 会重写为 Stacktory 地址，工具链安装和更新也走内网。
- **来源组回退**：配置 `group` 后，服务按成员顺序尝试上游；每个成功响应只绑定一个来源，不合并不同来源的版本集合。
- **完整性校验**：npm integrity、PyPI 文件摘要、NuGet 包摘要、Cargo 包校验、Docker digest、Winget `InstallerSha256` 等校验不通过的内容不会进入缓存。
- **本地缓存与审计**：制品命中缓存后直接返回；缓存内容和访问、来源选择审计写到本地文件，不需要数据库。
- **只读、免客户端凭据**：服务本身不要求客户端账号或 token，也不提供写入接口，减少凭据管理和误发布风险。
- **轻量部署**：单个 Rust 二进制，文档页和静态资源内嵌；一个进程、一份配置和本地目录即可运行，Docker Compose 是可选方式。

## 支持的技术栈

| 技术栈 | 支持内容 | 客户端配置 |
| --- | --- | --- |
| Rustup | 安装、更新 Rust 工具链和组件 | `RUSTUP_DIST_SERVER=<服务地址>/mirror/rustup/group` |
| Cargo | 下载 crates.io 依赖 | Sparse registry `<服务地址>/mirror/cargo/group/` |
| npm / Bun | 安装 npm 依赖 | registry `<服务地址>/mirror/npm/group/` |
| PyPI | 通过 Simple API 只读安装 Python 包 | `index-url=<服务地址>/mirror/pypi/group/simple/` |
| NuGet | 只读 restore、查询和漏洞数据 | source `<服务地址>/mirror/nuget/group/v3/index.json` |
| Docker | 只读拉取 Docker 镜像 | `registry-mirrors` 填 Docker source 配置的 `public_url` |
| Unity UPM | 安装 UPM 包 | scoped registry `<服务地址>/mirror/upm/group` |
| VS Code | 搜索和安装扩展，需要专用 VS Code 版本 | 扩展面板，配置专用版本的 `product.json` |
| Winget | 搜索和安装 Windows 软件；协议已实现，真实客户端待验收 | `Microsoft.Rest` source `<服务地址>/mirror/winget/group/` |

`<服务地址>` 对应 `config.toml` 的 `server.public_url`；`group` 是示例配置中的入口名，实际入口以你的 `sources` 配置为准。

## 部署

1. 本地运行时，复制 `deploy/app/config.example.toml` 为仓库根目录的 `config.toml`（该文件已被忽略）；容器运行时复制为 `deploy/app/config.toml`。修改 `[server].public_url` 和 `[storage]`（本地改成宿主机目录，容器内保持 `/app/...`），按需启用 `[cargo]`、`[npm]`、`[pypi]` 等生态的 `sources`；至少保留 `[server]`、`[storage]`、`[rustup]`。
2. 本地运行在仓库根目录执行 `cargo run`；`config.toml` 只从当前工作目录读取。容器运行按 `deploy/docker-compose.yml` 创建网络、准备数据目录，再启动 Compose。
3. 打开 `<public_url>/docs`，确认文档页能列出已配置的生态和入口。

未配置的生态不会注册路由。

## 使用

1. 在文档页选择技术栈，页面会按当前 `sources` 生成手动步骤、可复制提示词和验证命令。
2. 入口名取自该生态某个 source 的 `name`；示例配置提供 `group`，文档页会优先使用 `group`，没有 `group` 时使用第一个 source。
3. 按页面配置客户端并执行验证命令，例如 `npm ping`、`cargo fetch`、`python -m pip download`、`dotnet restore`、`docker info` 或 `winget search`。

以 npm 为例，`<服务地址>` 为 `server.public_url`，入口为 `group`：

```text
npm config set registry <服务地址>/mirror/npm/group/
npm ping
```

## 适用范围

- 适合内网开发机统一下载依赖、工具链和容器镜像，也适合上游不稳定、需要按顺序回退的场景。
- 只读：不提供任何生态的发布、上传、删除、账号、token 或权限管理。
- 上游认证：只支持匿名可读的上游；需要登录、token 或私有认证的上游不在当前范围。
- 能力边界：只负责只读下载，不承担制品发布、审批、细粒度权限和高可用治理。
- 部分能力有明确边界：PyPI 只支持 Simple API 安装，NuGet 只支持 restore、查询和漏洞数据，Docker 只支持拉取，Winget 的真实客户端仍待验收，VS Code 需要专用版本。

## 文档

部署模板见 `deploy/`，完整配置示例见 `deploy/app/config.example.toml`，架构与验证记录见 `docs/`。
