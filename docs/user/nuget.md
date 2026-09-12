# NuGet

## 状态

NuGet 适配器当前尚未向用户开放。NuGet service index 的部分实现不代表 `dotnet restore` 已经可以使用 Stacktory。

请不要把 Stacktory 添加为 NuGet source：

```text
dotnet nuget add source https://stacktory.example.internal/mirror/nuget/group/v3/index.json --name stacktory
```

适配器开放后，管理员会提供正式的 NuGet source 地址和 restore 配置。
