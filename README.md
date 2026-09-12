# Stacktory

Stacktory 为公司内网开发机提供统一的工具链和软件包下载入口。用户只需要使用管理员提供的地址和入口，不需要直接访问各个外部制品库。

## 当前可用能力

| 制品类型 | 状态 | 使用方式 |
| --- | --- | --- |
| Rustup | 可用 | 配置 `RUSTUP_DIST_SERVER` 后安装 Rust 工具链 |
| Cargo | 可用 | 配置 Cargo source replacement 后正常使用 Cargo |
| VS Code 扩展 | 可用 | 使用公司提供的专用 VS Code，在扩展面板中安装 |
| Docker 镜像 | 暂不可用 | 请勿配置 Docker |
| npm 包 | 可用 | 配置 npm registry 后使用 `npm install` 或 `npm ci` |
| Unity UPM 包 | 暂不可用 | 请勿配置 Unity Package Manager |
| PyPI 包 | 暂不可用 | 请勿配置 pip |
| NuGet 包 | 暂不可用 | 请勿配置 NuGet |

## 开始使用

请阅读 [Stacktory 用户文档](docs/user/README.md)。

管理员需要提供：

- Stacktory 服务地址。
- 对应制品类型的入口名称，通常是 `group`。
- 是否允许使用某个直接来源入口，例如 `official`。
- 公司提供的专用 VS Code 产品版本。

用户不要自行猜测入口名称、修改上游地址或把暂不可用的客户端配置到 Stacktory。
