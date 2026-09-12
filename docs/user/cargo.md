# Cargo

## 状态

Cargo Sparse Registry 和 crate 下载当前可用。

## 配置下载源

在 `%USERPROFILE%\.cargo\config.toml` 中配置。将地址替换为管理员提供的 Stacktory 地址和入口名称：

```toml
[source.crates-io]
replace-with = "stacktory"

[source.stacktory]
registry = "sparse+https://stacktory.example.internal/mirror/cargo/group/"
```

只对一个项目生效时，也可以将相同内容放在项目目录的 `.cargo/config.toml`。

使用直接入口时，将 `group` 替换为管理员提供的入口名称：

```toml
[source.crates-io]
replace-with = "stacktory"

[source.stacktory]
registry = "sparse+https://stacktory.example.internal/mirror/cargo/official/"
```

## 下载依赖和构建

配置完成后，Cargo 命令不需要改变：

```text
cargo fetch
cargo build
cargo test
```

安装 crates.io 上的命令行工具：

```text
cargo install ripgrep
```

按锁文件安装：

```text
cargo install --locked ripgrep
```

当前 Cargo 使用范围是依赖下载和只读 Sparse Registry，不包含软件包发布。
