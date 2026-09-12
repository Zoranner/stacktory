# Unity UPM

## 配置

管理员提供 Stacktory 服务地址和 UPM 入口名称后，在 Unity 项目的
`Packages/manifest.json` 中配置 scoped registry：

```json
{
  "scopedRegistries": [
    {
      "name": "Stacktory",
      "url": "https://stacktory.example.internal/mirror/upm/group",
      "scopes": [
        "com.company"
      ]
    }
  ]
}
```

`scopes` 属于 Unity 项目的本地配置，用于决定哪些 package scope 发送到 Stacktory；
Stacktory 服务端只提供 `/mirror/upm/{name}/...` registry 入口，不维护 scope 列表。

## 只读边界

UPM 使用 npm-compatible metadata 和 tarball 下载协议，但使用 Unity package name，
例如 `com.company.package`。UPM 与 npm 使用独立的入口、缓存和审计生态。

当前只支持 package metadata 查询和 tarball 下载，不支持 package 发布、删除、账号、
token、权限管理、Asset Store、Unity Editor 或 Git URL 包代理。
