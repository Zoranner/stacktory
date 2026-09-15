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
