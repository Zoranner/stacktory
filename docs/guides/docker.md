# Docker Registry V2

## 状态

Stacktory 已提供 Docker Registry HTTP API V2 的公开只读适配器，入口为：

```text
/mirror/docker/{name}/v2/...
```

首期支持 manifest、tag、digest、manifest list、OCI image index、image config
和 layer blob。只支持不需要认证的公开 Registry；当前没有实现 token 获取、账号
管理或认证转发。需要 Bearer challenge、私有仓库或对象存储重定向的来源，必须等
管理员单独确认配置边界后再开放。

Stacktory 不向 Docker 客户端返回上游 Registry host。上游协议请求由服务端生成：

```text
/v2/{repository}/manifests/{reference}
/v2/{repository}/blobs/{digest}
```

真实 Docker/OCI 客户端的路径前缀兼容性和 pull 联调需要在管理员提供客户端及
运行环境后单独验收；本工作区未安装 `docker`、`oras`、`nerdctl` 或 `skopeo`。
