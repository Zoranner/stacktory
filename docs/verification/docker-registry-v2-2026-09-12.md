# Docker Registry V2 Verification

## Local Protocol Tests

本地 HTTP 上游测试覆盖：

- `GET /mirror/docker/{name}/v2/` Registry V2 能力探测。
- tag manifest 和 digest manifest。
- Docker manifest list 与 OCI image index。
- image config 和 layer blob。
- 上游请求路径 `/v2/{repository}/manifests/{reference}`。
- 上游请求路径 `/v2/{repository}/blobs/{digest}`。
- `Accept`、`Content-Type`、`Docker-Content-Digest`。
- 完整 blob 校验后的 Range 响应、`206 Content-Range` 和非法 Range 的 `416`。
- digest 校验失败不返回成功内容，也不写入 artifact 缓存。
- group 按 members 顺序回退，并记录实际 `selected_proxy`。
- group 不合并不同 proxy 的 manifest 或版本集合。
- group 和 direct proxy 在实际 selected proxy 相同的情况下复用 verified content cache。

执行命令：

```text
cargo test --test docker_path_test
cargo test --test docker_manifest_test
cargo test --test docker_artifact_test
cargo test --test docker_cache_test
cargo test --test docker_group_test
cargo test --test docker_registration_test
```

## Static And Rust Verification

已执行：

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

## Real Client Boundary

2026 年 9 月 12 日在当前 Windows 工作区检查了以下公开 Docker/OCI 客户端：

- `docker`：未安装或不在 PATH。
- `oras`：未安装或不在 PATH。
- `nerdctl`：未安装或不在 PATH。
- `skopeo`：未安装或不在 PATH。

因此本次没有执行真实客户端 pull。该结果不等同于真实 Docker/OCI 客户端
联调通过；当前证据只覆盖本地 HTTP 协议测试、主程序注册和 Rust 静态检查。
