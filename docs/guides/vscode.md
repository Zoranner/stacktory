# VS Code 扩展

## 状态

Microsoft Marketplace 扩展搜索、查看和安装当前可用。

## 使用条件

扩展市场需要使用公司提供的专用 VS Code 产品版本。扩展市场地址已经由管理员写入产品配置，用户不需要在普通 VS Code 的 `settings.json` 中填写 Stacktory 地址。

普通 VS Code 不支持通过用户设置切换到本服务。请使用公司提供的专用版本，否则可能出现登录提示或扩展市场不可用。

## 源配置文档

打开 [Stacktory | 使用说明](https://pkg.rd.kim/docs)，进入 VS Code 详情页，复制提示词
发给智能体。提示词要求先备份当前 `product.json`，只修改扩展市场地址，并在修改后要求
完全重启 VS Code。

页面提供的提示词不包含凭据，也不会自动执行远程脚本。

## 安装扩展

1. 打开公司提供的专用 VS Code。
2. 打开左侧扩展面板。
3. 搜索扩展名称。
4. 打开扩展详情。
5. 点击安装或更新。

用户不需要手动下载 VSIX，也不需要登录 Microsoft Marketplace。
