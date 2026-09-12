# NuGet

## 状态

Stacktory 已实现 NuGet V3 所需的只读 service index、Search、package version
index、registration 和 `.nupkg` 下载路由。真实 .NET 客户端已通过官方源
和本地 HTTP 上游验证。

## 配置 NuGet source

将管理员提供的 Stacktory 地址和入口名称替换到下面命令中。入口通常是
`group`：

```text
dotnet nuget add source https://stacktory.example.internal/mirror/nuget/group/v3/index.json --name stacktory
dotnet restore --source stacktory
```

也可以在项目或解决方案目录的 `NuGet.Config` 中配置：

```xml
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="stacktory" value="https://stacktory.example.internal/mirror/nuget/group/v3/index.json" />
  </packageSources>
</configuration>
```

## 支持范围

适配器只支持 restore 所需的以下入口：

```text
GET /mirror/nuget/{name}/v3/index.json
GET /mirror/nuget/{name}/search/query?{query}
GET /mirror/nuget/{name}/package/{id}/index.json
GET /mirror/nuget/{name}/registration/{*path}
GET /mirror/nuget/{name}/package/{id}/{version}/{id}.{version}.nupkg
```

不支持 push、delete、unlist、API key、登录、权限、账号、symbol server
或其他写入型 NuGet API。用户不需要也不应配置上游 NuGet 地址；
上游 service index 由 Stacktory 管理员在服务端配置。
