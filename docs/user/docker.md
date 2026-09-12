# Docker Registry

## 状态

Docker Registry 当前尚未向用户开放。

虽然配置模板中已经包含 Docker source，但 Docker 客户端目前不能使用 Stacktory 拉取镜像。请不要修改 Docker daemon 的 `registry-mirrors`，也不要把 Docker 配置为 Stacktory 地址。

Docker 适配器开放后，管理员会提供专用的 registry 地址和 `docker pull` 使用方式。
