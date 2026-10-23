---
id: npm
group: Node.js
title: npm
summary: 通过 npm-compatible Registry 安装依赖。
ecosystem: npm
---
## 提示词
请把本机 npm 的默认 registry 切换到 Stacktory。

目标地址：{endpoint}
要求：
1. 先读取用户级 .npmrc，确认当前 registry 和已有配置；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只修改 registry，不删除其他配置项；
4. 保持幂等，重复执行不要重复追加；
5. 执行 npm ping --registry {endpoint} 验证；
6. 不写入 token、密码或认证信息；
7. 如果路径、权限或配置格式不确定时停止，不要猜测。

## 手动步骤
配置文件：用户级 .npmrc
Windows 通常为 %USERPROFILE%\.npmrc

npm config set registry {endpoint}

也可以直接写入 .npmrc：
registry={endpoint}

## 验证
npm ping --registry {endpoint}

## 注意
- 项目级 .npmrc 可能覆盖用户级配置。
- npm 和 Bun 共用 npm-compatible Registry，但客户端配置方式不同。
- Group 按成员顺序回退；Proxy 只访问自身，不会自动切换到兄弟来源。
