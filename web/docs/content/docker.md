---
id: docker
group: Docker
title: Docker
summary: 将 Docker 镜像源指向 Stacktory 的 Docker 入口。
ecosystem: docker
---
## 提示词
请把本机 Docker 配置为使用管理员提供的 Stacktory Docker 入口。

要求：
1. 先向用户确认要使用的 Docker 入口和客户端地址，不要猜测；
2. 确认当前用户具备修改 Docker daemon 配置的管理员权限；
3. 读取当前 daemon 配置并备份，Linux 通常是 /etc/docker/daemon.json；
4. 只修改 registry-mirrors，保留其他 daemon 配置；
5. 保持幂等，不重复追加；
6. 修改后重启 Docker，执行 docker info 检查 Registry Mirrors；
7. 如果地址、权限或配置路径不确定时停止，不要猜测。

## 手动步骤
配置文件：Docker Desktop 的 daemon.json，或 Linux 的 /etc/docker/daemon.json
地址：由管理员提供，不要猜测。

{
  "registry-mirrors": ["管理员提供的地址"]
}

## 验证
docker info

## 注意
- 需要管理员权限，修改后必须重启 Docker。
- 不要修改无关 daemon 配置；不要写入账号、密码或 token。
- 页面列出的 Group/Proxy 入口和 CLI 端口来自当前服务配置。
