# Stacktory

Stacktory 为公司内网开发机提供统一的工具链和软件包下载入口。用户只需要使用管理员提供的地址和入口，不需要直接访问各个外部制品库。

## 当前可用能力

| 制品类型 | 状态 | 使用方式 |
| --- | --- | --- |
| Rustup | 可用 | 配置 `RUSTUP_DIST_SERVER` 后安装 Rust 工具链 |
| Cargo | 可用 | 配置 Cargo source replacement 后正常使用 Cargo |
| VS Code 扩展 | 可用 | 使用公司提供的专用 VS Code，在扩展面板中安装 |
| Docker 镜像 | 公开无认证 Registry V2 只读可用 | 使用 source 的独立 CLI 端口或 `/mirror/docker/{name}/v2/...` |
| npm 包 | 可用 | 配置 npm registry 后使用 `npm install` 或 `npm ci` |
| Unity UPM 包 | 可用 | 在 Unity 项目中配置 Stacktory scoped registry |
| PyPI 包 | 暂不可用 | 请勿配置 pip |
| NuGet 包 | 只读 restore、查询和漏洞数据可用 | 配置 NuGet source 指向 `/mirror/nuget/{name}/v3/index.json` |

## 开始使用

请阅读 [Stacktory 使用指南](docs/guides/README.md)。

管理员需要提供：

- Stacktory 服务地址。
- 对应制品类型的入口名称，通常是 `group`。
- 是否允许使用某个直接来源入口，例如 `official`。
- 公司提供的专用 VS Code 产品版本。

用户不要自行猜测入口名称或修改上游地址；NuGet 仅支持 restore 所需的只读接口。

## 本地运行

复制 `deploy/app/config.example.toml` 为仓库根目录的 `config.toml`（该文件已被忽略，不会提交），
按本机情况修改监听地址、`root_url`、数据目录和日志目录，然后在仓库根目录启动服务：

```text
cargo run
```

`config.toml` 只从当前工作目录读取；容器部署请使用 `deploy/app/config.example.toml`。

## 出站代理

如果服务所在网络访问上游制品库必须经过 HTTP/HTTPS 代理，在 `config.toml` 顶层增加：

```toml
proxy_url = "http://proxy.example.internal:8080"
proxy_no_proxy = "localhost,127.0.0.1,.internal"
```

`proxy_url` 会应用于 Rustup、Cargo、npm、NuGet、UPM、VS Code 和 Docker 的所有上游请求。
只支持 HTTP/HTTPS 代理地址；`proxy_no_proxy` 可选，用于指定不经过代理的主机或域名。
未配置 `proxy_no_proxy` 时会读取 `NO_PROXY` 环境变量；如果两者都存在，`NO_PROXY` 优先。
容器内配置时，`proxy_url` 必须是容器可访问的地址；宿主机的 `127.0.0.1` 在容器内通常无效。

## 容器部署

在仓库根目录执行：

```text
docker network create stacktory
mkdir -p deploy/app/data deploy/app/logs
cp deploy/app/config.example.toml deploy/app/config.toml
chown -R 1000:1000 deploy/app/data deploy/app/logs
docker compose -f deploy/docker-compose.yml up -d
```

`chown` 在 Linux 宿主机上可能需要 `sudo`；容器以 UID 1000 运行，`deploy/app/data` 和
`deploy/app/logs` 必须对该用户可写。`deploy/app/config.toml` 中的 `data_dir`、
`log_dir` 应分别保持为 `/app/data`、`/app/logs`。

compose 默认发布主服务端口 `18761` 和 Docker `group` 入口的 `19000`。如果需要为其他
Docker source 开放标准 CLI 端口，在 `deploy/app/config.toml` 中设置对应
`listen_port`，并在 `deploy/docker-compose.yml` 中增加相同的端口映射。

## 缓存失效

修改 `root_url` 后，服务首次启动会清理旧的重写元数据和 Rustup 快照缓存，
但保留制品缓存和审计日志。
