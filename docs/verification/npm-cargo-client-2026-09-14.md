# npm 与 Cargo 真实客户端验证

## 环境

- Windows 工作区，Stacktory 以 `target/debug/stacktory.exe` 启动，监听
  `127.0.0.1` 的独立端口，`data_dir` 为临时目录。
- `npm 10.9.8`、Cargo 1.98.0（随本机 Rust 工具链）。
- 两个生态都使用 `group` 入口，`group` 只有一个 `proxy` 成员。

## npm CLI

项目 `.npmrc` 指向 `http://127.0.0.1:18801/mirror/npm/group/`，依赖
`is-odd@3.0.1`。

```text
npm install --no-audit --no-fund
```

结果：

- 首次安装成功，`node_modules` 含 `is-odd` 与传递依赖 `is-number`，耗时 5 秒。
- 审计记录 `npm_metadata_published`（`is-odd`、`is-number`）和两条
  `npm_artifact_published`，`selected_proxy` 均为 `npmjs`。
- 缓存落盘为 `npm/artifacts/npmjs/objects/group/{package}/{version}/tarball/{sha256}`
  与 `npm/metadata/group/{package}/`。
- 第二个客户端目录使用同一服务再次安装，耗时 2 秒，审计记录两条
  `npm_metadata_read` 且 `cache_status` 为 `hit`，未再向上游发布。

## Cargo 客户端

项目 `.cargo/config.toml` 使用 source replacement：

```toml
[source.crates-io]
replace-with = "stacktory"

[source.stacktory]
registry = "sparse+http://127.0.0.1:18802/mirror/cargo/group/"
```

依赖 `itoa = "1.0.11"`，`CARGO_HOME` 指向项目内的临时目录，避免影响日常缓存。

```text
cargo fetch
cargo build
```

结果：

- `cargo fetch` 更新 `stacktory` index 并下载 `itoa v1.0.18`，退出码 0。
- `cargo build` 编译通过，运行生成的二进制输出 `42`，说明经 Stacktory 获取的
  crate 通过 Cargo 自身校验。
- 审计记录 `cargo_index_published`（`it/oa/itoa`）、后续 `cargo_index_read`
  `cache_status` 为 `hit`、`cargo_artifact_read` 首次为 `miss`，随后
  `cargo_artifact_published` 发布 `crates/itoa/1.0.18.crate`。
- 缓存落盘为 `cargo/artifacts/official/objects/crates/itoa/1.0.18.crate`、
  `cargo/metadata/group/index/it/oa/itoa` 和同目录的 `.source` 来源绑定。
- 使用另一个空 `CARGO_HOME` 再次 `cargo fetch`，审计显示 index 与 crate 都是
  `cache_status` 为 `hit`，未重新访问 crates.io。

## 仍未验证的边界

- Cargo 面向 Nexus 等目标上游、Git Index 和 `cargo install` 未验证。
- Unity Package Manager 需要 Unity Editor，按仓库规则不启动。
- Docker/OCI 真实客户端：本机没有 `docker`、`oras`、`nerdctl` 或 `skopeo`。
- VS Code 扩展侧栏安装仍需隔离的运行时副本单独验收。
- 生产部署（Compose、镜像、反向代理下的真实地址）未验证；本次结论只覆盖本机
  明文 HTTP 入口和本地临时数据目录。
