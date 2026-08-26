# Rustup Mirror MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 构建一个可运行的 Rustup 内网镜像适配器，可原子发布重写 manifest 并安全缓存校验 dist 制品。

**Architecture:** HTTP 路由只将已验证的 Rustup 原生路径传入适配器；适配器从配置顺序的固定上游获取数据，并把快照和制品分别交给本地存储。URL、散列和访问控制均在协议边界验证，公共层不代理客户端指定地址。

**Out of scope:** 官网和 `/docs/{stack}` 使用教程属于后续只读文档站，不在本 Rustup 代理 MVP 内实现。

**Tech Stack:** Rust 2024、Axum、Tokio、Reqwest、Serde/TOML、SHA-256、Tempfile。

---

### Task 1: 初始化服务与配置校验

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/config.rs`
- Create: `tests/config_test.rs`

- [ ] **Step 1: 写出失败的配置测试**

```rust
#[test]
fn accepts_public_production_configuration() {
    let config = Config::from_toml(PUBLIC_PRODUCTION).unwrap();
    assert_eq!(config.listen.ip().to_string(), "0.0.0.0");
}
```

- [ ] **Step 2: 运行测试确认其因 `Config` 尚不存在而失败**

Run: `cargo test --test config_test rejects_anonymous_production_configuration`

- [ ] **Step 3: 实现最小配置类型和启动入口**

```rust
pub struct Config { pub listen: SocketAddr, pub data_dir: PathBuf, /* ... */ }
impl Config { pub fn from_toml(input: &str) -> Result<Self, ConfigError> { /* validate */ } }
```

- [ ] **Step 4: 运行配置测试并提交基础服务**

Run: `cargo test --test config_test`
Expected: PASS

### Task 2: 建立 Rustup 路径与 manifest 转换契约

**Files:**
- Create: `src/rustup/mod.rs`
- Create: `src/rustup/path.rs`
- Create: `src/rustup/manifest.rs`
- Test: `tests/rustup_path_test.rs`
- Test: `tests/manifest_test.rs`

- [ ] **Step 1: 先写路径和 URL 重写的失败测试**

```rust
#[test]
fn rejects_encoded_parent_segment() {
    assert!(parse_dist_path("%2e%2e/channel-rust-stable.toml").is_err());
}

#[test]
fn rewrites_only_configured_dist_urls() {
    let rewritten = rewrite_manifest(SOURCE, &policy, &public_base).unwrap();
    assert!(rewritten.contains("/mirror/rustup/dist/rust-std-"));
}
```

- [ ] **Step 2: 运行测试，确认接口未实现**

Run: `cargo test --test rustup_path_test --test manifest_test`

- [ ] **Step 3: 实现路径白名单和递归 TOML URL 重写**

```rust
pub fn parse_dist_path(encoded: &str) -> Result<DistPath, RustupError>;
pub fn rewrite_manifest(source: &str, policy: &UrlPolicy, public_base: &Url)
    -> Result<RewrittenManifest, RustupError>;
```

- [ ] **Step 4: 运行测试并提交协议纯函数**

Run: `cargo test --test rustup_path_test --test manifest_test`
Expected: PASS

### Task 3: 实现快照发布与受校验缓存

**Files:**
- Create: `src/storage.rs`
- Create: `src/cache.rs`
- Test: `tests/storage_test.rs`
- Test: `tests/cache_test.rs`

- [ ] **Step 1: 写出失败的快照一致性和散列不符测试**

```rust
#[tokio::test]
async fn current_snapshot_returns_manifest_and_its_checksum() { /* publish then read */ }

#[tokio::test]
async fn cache_does_not_publish_file_with_wrong_sha256() { /* download fixture */ }
```

- [ ] **Step 2: 运行测试，确认存储类型尚不存在**

Run: `cargo test --test storage_test --test cache_test`

- [ ] **Step 3: 实现版本化快照、当前指针和临时文件校验发布**

```rust
pub async fn publish_snapshot(&self, snapshot: PublishedSnapshot) -> Result<(), StorageError>;
pub async fn get_or_fetch(&self, path: &DistPath, expected_sha256: &str)
    -> Result<CachedArtifact, CacheError>;
```

- [ ] **Step 4: 运行测试并提交本地状态层**

Run: `cargo test --test storage_test --test cache_test`
Expected: PASS

### Task 4: 连接固定上游与 HTTP 服务

**Files:**
- Create: `src/upstream.rs`
- Create: `src/http.rs`
- Modify: `src/main.rs`
- Test: `tests/http_test.rs`

- [ ] **Step 1: 写出失败的上游回退与 dist 缓存 HTTP 测试**

```rust
#[tokio::test]
async fn dist_request_falls_back_and_uses_cached_verified_file() { /* two local upstreams */ }
```

- [ ] **Step 2: 运行测试，确认路由尚未实现**

Run: `cargo test --test http_test dist_request_falls_back_and_uses_cached_verified_file`

- [ ] **Step 3: 实现固定上游客户端、配置驱动的刷新任务和三类 Rustup 路由**

```rust
GET /healthz
GET /mirror/rustup/dist/channel-rust-{channel}.toml{.sha256}
GET /mirror/rustup/dist/{*path}
```

- [ ] **Step 4: 执行完整验证并提交 MVP**

Run: `cargo fmt --all; cargo clippy --all-targets --all-features -- -D warnings; cargo test`
Expected: all commands exit 0
