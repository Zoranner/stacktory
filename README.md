# Stacktory

Stacktory 为公司内网开发机提供统一的工具链和软件包下载入口。用户只需要使用管理员提供的地址和入口，不需要直接访问各个外部制品库。

## 当前可用能力

| 制品类型 | 状态 | 使用方式 |
| --- | --- | --- |
| Rustup | 可用 | 配置 `RUSTUP_DIST_SERVER` 后安装 Rust 工具链 |
| Cargo | 可用 | 配置 Cargo source replacement 后正常使用 Cargo |
| VS Code 扩展 | 可用 | 使用公司提供的专用 VS Code，在扩展面板中安装 |
| Docker 镜像 | 公开无认证 Registry V2 只读可用 | 使用 source 的独立 Registry 入口或 `/mirror/docker/{name}/v2/...` |
| npm 包 | 可用 | 配置 npm registry 后使用 `npm install` 或 `npm ci` |
| Unity UPM 包 | 可用 | 在 Unity 项目中配置 Stacktory scoped registry |
| PyPI 包 | 暂不可用 | 请勿配置 pip |
| NuGet 包 | 只读 restore、查询和漏洞数据可用 | 配置 NuGet source 指向 `/mirror/nuget/{name}/v3/index.json` |

示例配置默认启用 Rustup、Docker 和 npm；Cargo、NuGet、UPM、VS Code 需要按同样的
来源结构增加对应生态段，未配置的生态不会注册路由。

## 开始使用

打开 [Stacktory | 文档](https://pkg.rd.kim/docs)。
页面按技术栈提供手动步骤和可复制提示词，不执行远程脚本。

管理员需要提供：

- Stacktory 服务地址。
- 对应制品类型的入口名称，通常是 `group`。
- 是否允许使用某个直接来源入口，例如 `official`。
- 公司提供的专用 VS Code 产品版本。

用户不要自行猜测入口名称或修改上游地址；NuGet 仅支持 restore 所需的只读接口。

## 本地运行

复制 `deploy/app/config.example.toml` 为仓库根目录的 `config.toml`（该文件已被忽略，不会提交），
按本机情况修改 `[server]`、`[storage]`，然后在仓库根目录启动服务：

```text
cargo run
```

`config.toml` 只从当前工作目录读取；容器部署请使用 `deploy/app/config.example.toml`。

`server.address` 和 `server.port` 是进程监听地址，`server.public_url` 是客户端实际访问的
地址。`public_url` 可写单个字符串或数组；数组第一项是默认地址，请求按 `Host` 与
`X-Forwarded-Proto` 匹配数组项，匹配不到时使用第一项，不会反射任意请求 Host。
反向代理需要转发 `Host` 和 `X-Forwarded-Proto`。需要同时提供 HTTP 与 HTTPS 时，把两个
地址都写进数组；修改 `server.public_url` 后缓存不需要重建。

```nginx
proxy_set_header Host $host;
proxy_set_header X-Forwarded-Proto $scheme;
```

服务端口只应对反向代理开放；不要把 `server.port` 直接暴露给不受信任的客户端。

配置 schema 不保留旧键兼容，旧配置会因未知字段在启动时被拒绝。

## 出站代理

如果服务所在网络访问上游制品库必须经过 HTTP/HTTPS 代理，在 `config.toml` 中增加：

```toml
[proxy]
url = "http://proxy.example.internal:8080"
no_proxy = "localhost,127.0.0.1,.internal"
```

`proxy.url` 会应用于 Rustup、Cargo、npm、NuGet、UPM、VS Code 和 Docker 的所有上游请求。
只支持 HTTP/HTTPS 代理地址；`proxy.no_proxy` 可选，用于指定不经过代理的主机或域名。
配置了 `proxy.no_proxy` 时以配置为准；未配置时才读取 `NO_PROXY` 环境变量。
容器内配置时，`proxy.url` 必须是容器可访问的地址；宿主机的 `127.0.0.1` 在容器内通常无效。

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
`deploy/app/logs` 必须对该用户可写。`deploy/app/config.toml` 中的
`storage.data_dir`、`storage.log_dir` 应分别保持为 `/app/data`、`/app/logs`。

compose 默认发布主服务端口 `18000` 和 Docker `group` 入口的 `19000`。如果需要为其他
Docker source 开放标准 Registry 入口，在 `deploy/app/config.toml` 中同时设置
`listen_port` 和 `public_url`，并在 `deploy/docker-compose.yml` 中增加相同的端口映射。
Docker source 的 `public_url` 是单值，必须是客户端实际访问的完整 Registry 地址。
compose 会通过容器内 `127.0.0.1:18000` 的 TCP 连接检查服务是否在监听。
需要区分存活和就绪时，`/healthz` 只报告进程可响应，`/readyz` 会检查数据目录和
日志目录可写；就绪检查失败返回 `503`。

## 缓存失效

修改 `server.public_url` 后，服务只更新本地标记，不删除 metadata、快照或制品缓存。
npm/UPM metadata、Rustup manifest 和 NuGet JSON metadata 会在读取时按当前
地址重新渲染或重写；NuGet 缓存主体保留生成时的公开基地址，请求匹配到其他地址时
只在响应中重写。缺少公开地址标记的旧缓存按未命中重建。

修改同名 proxy 的 `base_url` 或 `allow_http` 后，NuGet 和 Docker 中保存上游
绝对地址或可变引用的 metadata 与来源绑定会按未命中重建；按 digest 或摘要校验的
制品内容仍可继续复用。

Cargo 索引命中超过 15 分钟后会从上游重新验证；上游删除 crate 时会清理本地索引。
NuGet 的 service index、registration、package index 和 package binding 同样在
命中超过 15 分钟后重新验证；上游返回 `404` 时清理对应 metadata，其他失败继续
提供最后一次有效缓存并记录 stale 审计。

如果 NuGet 上游删除并重新发布了同一个版本，可以在服务所在目录执行：

```text
stacktory invalidate-nuget-metadata --entry group
```

该命令只删除指定入口的 NuGet metadata，保留 package content；后续请求会重新
获取 service index、registration 和 package index。新 registration 提供摘要时
按新摘要重新校验包内容；未提供摘要且来源身份未变化时，旧 package content 不会
仅因执行该命令而失效。
