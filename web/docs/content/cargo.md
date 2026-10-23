---
id: cargo
group: Rust
title: Cargo
summary: 通过 Sparse Registry 下载 crates.io 依赖。
ecosystem: cargo
---
## 提示词
请把本机 Cargo 的 crates.io 源切换到 Stacktory。

目标地址：{endpoint}
要求：
1. 先读取 ~/.cargo/config.toml，确认文件位置、当前格式和已有 source；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只修改 crates.io 的 source replacement，不删除其他 source；
4. 保持幂等，重复执行不要重复追加配置；
5. 修改后在包含依赖的项目中执行 cargo fetch 验证；
6. 输出实际修改的文件、备份路径和验证结果；
7. 如果路径、权限或配置格式不确定时停止，不要猜测。

## 手动步骤
配置文件：~/.cargo/config.toml
Windows 通常为 %USERPROFILE%\.cargo\config.toml

[source.crates-io]
replace-with = "stacktory"

[source.stacktory]
registry = "sparse+{endpoint}"

## 验证
cargo fetch

## 注意
- 保留现有 source，不要覆盖无关配置；项目级 .cargo/config.toml 可能覆盖用户配置。
- Stacktory 只提供只读 Sparse Registry，不包含 crate 发布。
- Group 按成员顺序回退；Proxy 只访问自身，不会自动切换到兄弟来源。
