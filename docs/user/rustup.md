# Rustup

## 状态

Rustup 工具链和组件下载当前可用。

## 首次安装

首次安装 Rustup 时使用 Rust 官方安装程序。Stacktory 负责工具链和组件下载，不替代 Rustup 首次安装程序。

安装完成后确认命令可用：

```text
rustup --version
rustc --version
cargo --version
```

## 配置下载地址

将下面的地址替换为管理员提供的 Stacktory 地址和入口名称。

PowerShell 当前窗口配置：

```powershell
$env:RUSTUP_DIST_SERVER = "https://stacktory.example.internal/mirror/rustup/group"
```

Windows 用户级永久配置：

```powershell
[Environment]::SetEnvironmentVariable("RUSTUP_DIST_SERVER", "https://stacktory.example.internal/mirror/rustup/group", "User")
```

设置永久配置后重新打开终端。

## 安装工具链

```text
rustup toolchain install stable
rustup default stable
```

安装常用组件：

```text
rustup component add rustfmt clippy
```

安装目标平台：

```text
rustup target add x86_64-pc-windows-msvc
```

使用 nightly：

```text
rustup toolchain install nightly
rustup override set nightly
```

## 使用直接入口

如果管理员提供了直接来源入口，只替换地址最后的入口名称：

```powershell
$env:RUSTUP_DIST_SERVER = "https://stacktory.example.internal/mirror/rustup/official"
rustup toolchain install stable
```
