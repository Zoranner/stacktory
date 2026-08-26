# Stacktory

Stacktory 是面向公司内网开发机的研发制品与动态工具链供应平台。服务器按照固定、
配置化的上游顺序获取内容，开发机只访问 Stacktory 的内网地址；客户端不能传入任意
上游 URL。

当前 MVP 实现 Rustup 动态工具链镜像适配器。后续包与制品生态会继续位于统一的
`/mirror/{ecosystem}` 命名空间，但不在本阶段实现。

## Rustup 路由

| 路由 | 行为 |
| --- | --- |
| `GET /healthz` | 服务存活检查。 |
| `GET /mirror/rustup/dist/channel-rust-{channel}.toml` | 返回当前内部化的 channel manifest。 |
| `GET /mirror/rustup/dist/channel-rust-{channel}.toml.sha256` | 返回同一快照 manifest 的 SHA-256。 |
| `GET /mirror/rustup/dist/{path}` | 仅返回已发布 manifest 引用、上游内容摘要校验通过的 Rustup 制品。 |

channel manifest 缺失或过期时由请求触发刷新。同一 channel 的并发刷新会合并；刷新
失败但已有旧快照时继续返回旧快照。制品按 manifest 中的 SHA-256 校验后原子写入
`data_dir/rustup/artifacts/`，未知或非法路径不会触发上游请求。

Rustup 路由无需认证。运行审计追加到 `data_dir/audit/events.jsonl`，记录 manifest
发布、制品缓存和上游失败，但不记录上游响应正文。

## 部署

部署模板位于 `deploy/`：实际配置为 `deploy/app/config.toml`，运行数据为
`deploy/data/`，两者均不会提交。先从 `deploy/app/config.example.toml` 创建实际配置，
随后在 `deploy/` 下启动 Compose。

```text
docker compose up -d
```

生产配置必须使用 HTTPS 的 `public_base_url`。各 Rustup 上游默认要求 HTTPS；仅在
明确需要时，对对应条目设置 `require_https = false`。

内网开发机将 Rustup 分发服务器设置为：

```text
RUSTUP_DIST_SERVER=https://stacktory.example.internal/mirror/rustup
```

## 发布

`vX.Y.Z` 标签会触发 GitHub Actions。工作流要求标签版本与 `Cargo.toml` 一致，执行
格式化、Clippy、测试和锁定依赖的 release 构建，然后发布 amd64/arm64 GHCR 镜像及带
SHA-256 文件的离线镜像包。定义工作流本身不会创建标签、镜像或 Release。
