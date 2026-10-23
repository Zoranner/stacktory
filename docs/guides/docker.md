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

为支持标准 Docker Registry 客户端，Docker source 需要同时配置监听端口和客户端可见地址：

```toml
[[docker.sources]]
name = "group"
type = "group"
members = ["official", "ghcr", "quay", "microsoft", "aliyun", "xuanyuan", "unstructured"]
listen_port = 19000
public_url = "https://docker.example.com:19000"
```

配置后，该监听端口的根路径固定绑定到 `group`，客户端使用 `public_url`：

```text
docker.example.com:19000/v2/...
```

官方 Docker Hub 应始终放在 `members` 首位；官方源失败后，按声明顺序尝试备用
来源。示例中 `aliyun` 排在 `xuanyuan` 前面。

Docker group 会按成员顺序回退 `401`、`403`、`404`、`408`、`429`、`5xx` 和网络
失败；其他 `4xx` 直接返回。直接 source 入口不会向兄弟来源回退。

`public_url` 必须是客户端可访问的地址，不能包含路径、查询参数或片段；
它可以由外部 DNS、端口映射或反向代理发布。未配置这对字段的 source 仍只能通过
`/mirror/docker/{name}/v2/...` 访问。
仓库的 `deploy/app/config.example.toml` 已为 `group` 显式配置 `19000`，
`deploy/docker-compose.yml` 也映射了该端口；其他 source 需要按同样方式自行增加。

真实 Docker/OCI 客户端的路径前缀兼容性和 pull 联调需要在管理员提供客户端及
运行环境后单独验收；本工作区未安装 `docker`、`oras`、`nerdctl` 或 `skopeo`。
