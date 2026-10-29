---
id: winget
group: Windows
title: Winget
summary: 通过 Microsoft.Rest 只读源搜索和安装 Windows 软件。
ecosystem: winget
---
## 提示词
请把本机 Winget 的 Stacktory 来源配置好，不要改动现有的 winget 和 msstore 来源。

目标地址：{endpoint}
源类型：Microsoft.Rest
要求：
1. 先执行 winget --version 和 winget source list，记录当前来源和版本；
2. winget source add 需要管理员终端，请先确认当前终端已提升；
3. 先确认目标地址是 HTTPS、证书受信任，并且能访问 {endpoint}information；
4. 使用 winget source add --name stacktory "{endpoint}" --type Microsoft.Rest；
5. 如果 stacktory 已存在，停止并报告现有参数，不要覆盖或改名绕过；
6. 如果上游返回 source 协议，先完整展示协议内容，得到确认后才使用 --accept-source-agreements；
7. 不写入账号、token、密码或其他凭据；
8. 执行 winget source list --name stacktory 和 winget search --source stacktory 验证；
9. 报告实际执行的命令、返回码和未完成的步骤；权限、证书或客户端版本不确定时停止。

## 手动步骤
在管理员终端执行：

winget source add --name stacktory "{endpoint}" --type Microsoft.Rest
winget source update --name stacktory

如果只使用当前用户已有的来源，不要删除 winget 或 msstore。

可选的本地信任设置：
winget source edit --name stacktory --trust-level trusted

只有管理员确认 Stacktory 入口和上游来源后才设置 trusted。它是客户端本地属性，不是服务端能够声明的状态。

## 验证
winget source list --name stacktory
winget search --source stacktory
winget search --source stacktory --id <PackageIdentifier>
winget install --source stacktory --id <PackageIdentifier> --exact

## 注意
- Winget 的 Microsoft.Rest 来源必须使用 HTTPS；HTTP 地址会失败，自签名证书未加入受信任根时也会失败。
- source add 需要管理员终端；普通权限终端会失败，不能用 --disable-interactivity 绕过。
- 适配器只声明 REST 契约 1.0；上游不支持 1.0、返回认证要求或响应格式不符时会失败。
- 上游 source 协议会原样显示，客户端负责接受并记录；Stacktory 不会替客户端接受协议，也不会伪造 trusted。
- 以下情况会失败：入口未注册、source 名称已存在但参数不同、安装包主机未在 Stacktory 的 installer_hosts 中、manifest 缺少 InstallerSha256、下载内容 hash 不匹配。
- Group 按成员顺序回退；Proxy 只访问自身，不会自动切换到兄弟来源。
