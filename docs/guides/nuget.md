# NuGet

## 状态

Stacktory 已实现 NuGet V3 所需的只读 service index、Search、package version
index、registration、`.nupkg` 下载和漏洞数据路由。真实 .NET 客户端已通过
官方源和本地 HTTP 上游验证，包括 restore、搜索、过时版本、弃用和漏洞检查。

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

适配器只支持以下只读入口：

```text
GET /mirror/nuget/{name}/v3/index.json
GET /mirror/nuget/{name}/search/query?{query}
GET /mirror/nuget/{name}/package/{id}/index.json
GET /mirror/nuget/{name}/registration/{*path}
GET /mirror/nuget/{name}/package/{id}/{version}/{id}.{version}.nupkg
GET /mirror/nuget/{name}/vulnerability/index.json
GET /mirror/nuget/{name}/vulnerability/data/{file}.json
```

## 只读命令

还原、搜索、过时版本和漏洞检查都只读取 Stacktory：

```text
dotnet restore --source https://stacktory.example.internal/mirror/nuget/group/v3/index.json
dotnet package search Newtonsoft.Json --source https://stacktory.example.internal/mirror/nuget/group/v3/index.json --take 5
dotnet list package --outdated --source https://stacktory.example.internal/mirror/nuget/group/v3/index.json
dotnet list package --vulnerable --source https://stacktory.example.internal/mirror/nuget/group/v3/index.json
dotnet list package --deprecated --source https://stacktory.example.internal/mirror/nuget/group/v3/index.json
```

`dotnet restore` 的 NuGetAudit 与直接读取 VulnerabilityInfo 资源的客户端通过
`VulnerabilityInfo/6.7.0` 获取漏洞数据；上游未提供该资源时，Stacktory 不会在
service index 中暴露漏洞地址，客户端按无审计数据继续。`--vulnerable` 和
`--deprecated` 还会读取 registration 元数据，因此入口必须暴露 registration
resource；适配器已按客户端偏好选择信息最完整的 `RegistrationsBaseUrl/Versioned`
基址。

不支持 push、delete、unlist、API key、登录、权限、账号、symbol server
或其他写入型 NuGet API。用户不需要也不应配置上游 NuGet 地址；
上游 service index 由 Stacktory 管理员在服务端配置。

明文 HTTP 入口只用于内网联调和本地验证，需要在 `NuGet.Config` 的
`packageSources` 中显式打开 `allowInsecureConnections`；生产入口应使用
HTTPS，并让 `require_https` 保持生效。
