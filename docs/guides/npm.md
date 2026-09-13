# npm 生态

## 状态

npm Registry 适配器当前可用。npm 和 Bun 使用同一个 npm-compatible registry 入口，但客户端配置方式不同。

管理员提供 Stacktory 服务地址和 npm 入口名称后，将下面示例中的地址替换为管理员提供的地址：

```text
https://stacktory.example.internal/mirror/npm/group/
```

## npm 配置

使用 npm 命令配置当前用户的 registry：

```text
npm config set registry https://stacktory.example.internal/mirror/npm/group/
```

也可以在项目根目录的 `.npmrc` 中写入：

```text
registry=https://stacktory.example.internal/mirror/npm/group/
```

配置完成后，npm 命令不需要改变：

```text
npm install
npm ci
npm install <package>
```

普通包和 scoped package 都通过同一个 registry 入口访问，不需要手工拼接 `%2F`。

## Bun 配置

推荐在项目根目录、与 `package.json` 同级的位置创建 `bunfig.toml`：

```toml
[install]
registry = "https://stacktory.example.internal/mirror/npm/group/"
```

Bun 也支持在用户目录配置全局 Bun registry。Windows 用户目录通常是 `%USERPROFILE%\.bunfig.toml`；项目级 `bunfig.toml` 会覆盖全局配置。

配置完成后，使用 Bun 的正常命令：

```text
bun install
bun add <package>
bun remove <package>
bunx <package>
```

如果只希望某个 npm scope 使用 Stacktory，可以在 `bunfig.toml` 中配置：

```toml
[install.scopes]
"@company" = "https://stacktory.example.internal/mirror/npm/group/"
```

将 `@company` 和地址替换为管理员提供的 scope 与入口。Bun 官方支持通过 `bunfig.toml` 的 `[install] registry` 设置默认 registry，也支持通过 `[install.scopes]` 为指定 scope 设置 registry。

## 只读边界

当前支持 package metadata 查询、指定版本查询、dist-tags 查询、registry 搜索、ping 和
tarball 下载；不支持 `npm publish`、`unpublish`、dist-tag 修改、账号、token、权限管理
或其他写入 API。

metadata 命中某个上游来源后，版本对象中的 tarball 地址会改写为 Stacktory 内部地址。后续 tarball 请求优先使用同一个来源；来源不可用或完整性校验失败时，group 才按管理员声明的顺序回退。
