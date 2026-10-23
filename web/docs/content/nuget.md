---
id: nuget
group: .NET
title: NuGet
summary: 通过 NuGet V3 只读入口还原包和执行只读查询。
ecosystem: nuget
---
## 提示词
请把本机用户级 NuGet 配置的 Stacktory source 设置为只读源。

目标地址：{endpoint}
要求：
1. 先读取用户级 NuGet.Config，确认当前 source 和文件位置；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只添加或更新名为 stacktory 的 source，不删除其他 source；
4. 保持幂等，重复执行不要重复追加；
5. 不写入账号、token、密码或凭据；
6. 在已有项目中执行 dotnet nuget list source 和 dotnet restore --source stacktory 验证；
7. 如果路径、权限或配置格式不确定时停止，不要猜测。

## 手动步骤
配置文件：用户级 NuGet.Config
Windows 通常为 %APPDATA%\NuGet\NuGet.Config

dotnet nuget add source "{endpoint}" --name stacktory
dotnet nuget list source

## 验证
dotnet restore --source stacktory

## 注意
- 项目级 NuGet.Config 可能覆盖用户级配置。
- Stacktory 只支持 restore、搜索和漏洞数据等只读接口，不要配置上游地址或凭据。
- Group 按成员顺序回退；Proxy 只访问自身，不会自动切换到兄弟来源。
