# npm

## 配置

管理员提供 Stacktory 服务地址和 npm 入口名称后，将 registry 指向对应的命名入口：

```text
npm config set registry https://stacktory.example.internal/mirror/npm/group
```

随后可以继续使用：

```text
npm install
npm ci
```

普通包和 scoped package 都通过同一个 registry 入口访问。scoped package 的编码路径由 npm 客户端和 Stacktory 适配器处理，不需要手工拼接 `%2F`。

## 只读边界

当前只支持 metadata 查询和 tarball 下载，不支持 `npm publish`、`unpublish`、dist-tag 修改、账号、token、权限管理或其他写入 API。

metadata 命中某个上游来源后，版本对象中的 tarball 地址会改写为 Stacktory 内部地址。后续 tarball 请求优先使用同一个来源；来源不可用或完整性校验失败时，group 才按管理员声明的顺序回退。
