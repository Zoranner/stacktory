# Docker Registry V2

## 状态

Stacktory 已提供 Docker Registry HTTP API V2 的公开只读适配器，入口为：

```text
/mirror/docker/{name}/v2/...
```

首期支持 manifest、tag、digest、manifest list、OCI image index、image config
和 layer blob。Stacktory 会在服务端处理公开 Registry 的匿名 Bearer challenge、
安全的 blob 重定向和内容缓存，不要求用户提供账号、密码或 token。账号管理、私有
仓库凭据和写入型认证不在范围内。

只读查询支持：

```text
/mirror/docker/{name}/v2/_catalog
/mirror/docker/{name}/v2/{repository}/tags/list
/mirror/docker/{name}/search?q=<keyword>&n=<limit>
```

前两个接口对应 Registry V2 的仓库列表和 tag 列表。`search` 是 Stacktory 的
统一只读查询接口，服务端在一个已选中的 Registry `_catalog` 分页结果中按仓库名
过滤；它不实现 Docker Registry V1，也不把多个来源的结果合并。

Stacktory 不向 Docker 客户端返回上游 Registry host。上游协议请求由服务端生成：

```text
/v2/{repository}/manifests/{reference}
/v2/{repository}/blobs/{digest}
```

真实 Docker/OCI 客户端的路径前缀兼容性和 pull 联调需要在管理员提供客户端及
运行环境后单独验收；本工作区未安装 `docker`、`oras`、`nerdctl` 或 `skopeo`。
