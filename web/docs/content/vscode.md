---
id: vscode
group: VS Code
title: VS Code
summary: 将 VS Code 扩展市场切换到 Stacktory。
ecosystem: vscode
---
## 提示词
请把本机 VS Code 的扩展市场切换到 Stacktory。

目标地址：{endpoint}
要求：
1. 先通过 code --version 和安装目录定位当前 product.json，不要修改 settings.json；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只修改 extensionsGallery.serviceUrl、resourceUrlTemplate 和 extensionUrlTemplate；
4. 保留其他配置，保持幂等；
5. 修改后确认 product.json 可解析；
6. 要求完全退出并重新打开 VS Code；
7. 如果版本、commit、路径或权限不确定时停止，不要猜测。

## 手动步骤
配置文件：VS Code 安装目录/resources/app/product.json
部分版本位于版本目录/resources/app/product.json

"extensionsGallery": {
  "serviceUrl": "{endpoint}",
  "resourceUrlTemplate": "{endpoint}/resource/{publisher}/{name}/{version}/{path}",
  "extensionUrlTemplate": "{endpoint}/extension/{publisher}/{name}/latest"
}

## 验证
Get-Content -Raw product.json | ConvertFrom-Json

## 注意
- 修改前必须备份 product.json，VS Code 升级后可能覆盖该配置。
- 扩展市场应使用公司提供的专用 VS Code 产品版本；普通 VS Code 不要通过 settings.json 切换扩展市场。
- 修改 product.json 后需要完全退出并重新打开 VS Code。
- Group 优先尝试官方来源，失败后按成员顺序回退。
