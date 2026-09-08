# Stacktory

Stacktory 是面向公司内网开发机的研发制品与动态工具链供应平台。服务器按照固定、
配置化的上游顺序获取内容，开发机只访问 Stacktory 的内网地址；客户端不能传入任意
上游 URL。

当前 MVP 实现 Rustup 动态工具链镜像适配器，并提供 Rustup、Cargo 的命名来源配置
模型。Cargo 协议路由及其他包与制品生态会继续位于统一的 `/mirror/{ecosystem}`
命名空间，尚未实现的能力标记为后续工作。

## Rustup 路由

| 路由 | 行为 |
| --- | --- |
| `GET /healthz` | 服务存活检查。 |
| `GET /mirror/rustup/{name}/dist/channel-rust-{channel}.toml` | 返回指定命名入口的内部化 channel manifest。 |
| `GET /mirror/rustup/{name}/dist/channel-rust-{channel}.toml.sha256` | 返回同一快照 manifest 的 SHA-256。 |
| `GET /mirror/rustup/{name}/dist/{path}` | 返回指定命名入口已发布 manifest 引用且校验通过的 Rustup 制品。 |

channel manifest 缺失或过期时由请求触发刷新。同一 channel 的并发刷新会合并；刷新
失败但已有旧快照时继续返回旧快照。制品按 manifest 中的 SHA-256 校验后原子写入
`data_dir/rustup/artifacts/{name}/{channel}/{proxy}/`，未知或非法路径不会触发
上游请求。

Rustup 路由无需认证。运行审计追加到 `data_dir/audit/events.jsonl`，记录 manifest
发布、制品缓存和上游失败，但不记录上游响应正文。

## 部署

部署模板位于 `deploy/`：实际配置为 `deploy/app/config.toml`，运行数据为
`deploy/data/`，两者均不会提交。先从 `deploy/app/config.example.toml` 创建实际配置，
随后在 `deploy/` 下启动 Compose。

```text
docker compose up -d
```

生产配置必须使用 HTTPS 的 `root_url`。各 Rustup 上游默认要求 HTTPS；仅在
明确需要时，对对应条目设置 `require_https = false`。

内网开发机将 Rustup 分发服务器设置为：

```text
RUSTUP_DIST_SERVER=https://stacktory.example.internal/mirror/rustup/group
```

配置中的每个生态都使用命名 `sources`。`type = "proxy"` 声明一个上游地址，
`type = "group"` 按 `members` 顺序组合已声明的 proxy；未配置的生态保持禁用。
manifest 中的制品 URL 使用当前请求的 `/mirror/rustup/{name}` 根地址，并将 channel
作为 artifact URL 的绑定路径段。
group 刷新时选中的 proxy 会写入快照，后续 artifact 请求只访问该 proxy，不在
同一命名入口内混用其他 proxy。

Cargo source 配置已经纳入配置模型，协议路由仍待后续适配器实现。

## 发布

`vX.Y.Z` 标签会触发 GitHub Actions。工作流要求标签版本与 `Cargo.toml` 一致，执行
格式化、Clippy、测试和锁定依赖的 release 构建，然后发布 amd64/arm64 GHCR 镜像及带
SHA-256 文件的离线镜像包。定义工作流本身不会创建标签、镜像或 Release。
