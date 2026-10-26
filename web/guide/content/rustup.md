---
id: rustup
group: Rust
title: Rustup
summary: 通过 Stacktory 下载 Rust 工具链和组件。
ecosystem: rustup
---
## 提示词
请把本机 Rustup 的下载地址切换到 Stacktory。

目标地址：{endpoint}
要求：
1. 先确认 rustup 命令可用，并读取当前用户级 RUSTUP_DIST_SERVER；
2. 修改前记录原值，并在结果中报告恢复方式；
3. 只设置用户级 RUSTUP_DIST_SERVER，不修改其他 Rustup 配置；
4. 保持幂等，重复执行不要重复写入；
5. 重新打开终端后执行 rustup update stable，报告实际结果；
6. 如果路径、权限或当前配置不确定，停止并报告，不要猜测。

## 手动步骤
配置文件：用户级环境变量 RUSTUP_DIST_SERVER

PowerShell（Windows）：
[Environment]::SetEnvironmentVariable("RUSTUP_DIST_SERVER", "{endpoint}", "User")

设置后关闭并重新打开终端。

## 验证
rustup update stable

## 注意
- 设置的是用户级环境变量，修改后需要重新打开终端。
- Stacktory 负责工具链和组件下载，不替代 Rustup 首次安装程序。
- Group 按成员顺序回退；Proxy 只访问自身，不会自动切换到兄弟来源。
