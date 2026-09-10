# 多生态只读代理实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Stacktory 从 Rustup 专用镜像服务演进为可扩展的多生态只读代理，并先完成公共来源模型、命名路由、Rustup 迁移和 Cargo Sparse Index/crate 下载。

**Architecture:** 每个生态配置一组命名 `sources`，source 类型只有 `proxy` 和 `group`。proxy 对应一个远程仓库地址；group 显式按成员顺序回退。HTTP 路由使用 `/mirror/{ecosystem}/{name}/...`，适配器独立处理协议，公共层复用来源解析、HTTP 访问、缓存、完整性、原子写入和审计。MVP 不引入数据库，文件系统是配置以外的缓存、快照和审计权威存储。

**Tech Stack:** Rust 2024、Axum、Tokio、Reqwest、Serde、TOML、SHA-256、文件系统缓存、本地 HTTP 集成测试。

**Spec:** `docs/architecture/multi-ecosystem-readonly-proxy.md`

## Global Constraints

- 只读代理：不实现 publish、push、delete、账号、管理后台或远程配置中心。
- source 类型只能是 `proxy` 或 `group`。
- proxy 只配置一个 `base_url`；group 不配置 `base_url`，只引用同一生态的成员。
- 没有 group 时多个 proxy 彼此独立，不自动组合或回退。
- group 首期只能引用 proxy；成员非空、不可重复、必须存在且属于同一生态。
- source name 只需在当前生态内唯一，并直接形成 `/mirror/{ecosystem}/{name}/...` 路由段。
- 不保留当前 `/mirror/rustup/dist/**` 旧路由的兼容别名或过渡期。
- 不引入数据库；本地文件系统保存 metadata、artifact、Rustup 快照和审计事件。
- 不接受客户端传入任意上游 URL，不跟随未验证的跨来源重定向。
- Rustup、Cargo、npm、UPM、PyPI、NuGet 的协议逻辑必须保持适配器隔离。
- 未实现的生态不注册路由，不伪造协议支持。
- Rust 修改后执行 `cargo fmt --all` 和 `cargo clippy --all-targets --all-features -- -D warnings`；同时执行完整测试和锁定依赖 release build。

---

### Task 1: 配置模型与 source graph

**Files:**
- Modify: `src/config.rs`
- Modify: `src/lib.rs`
- Modify: `tests/config_test.rs`
- Create: `tests/source_config_test.rs`
- Modify: `deploy/app/config.example.toml`
- Modify: `README.md`
- Modify: `docs/architecture/rustup-mvp.md`

**Interfaces:**
- Produces `SourceType`, `SourceConfig`, `SourceGroup` 或等价的受限配置类型。
- Produces `SourceGraph`，负责按生态解析 proxy/group，并输出可执行的 proxy 成员顺序。
- `Config` 暴露 `rustup`、`cargo` 等生态配置；未配置的生态保持禁用。

- [ ] **Step 1: 写失败测试**

覆盖以下行为：

```rust
#[test]
fn accepts_proxy_and_group_sources() {}

#[test]
fn rejects_group_with_base_url() {}

#[test]
fn rejects_proxy_without_base_url() {}

#[test]
fn rejects_unknown_group_member() {}

#[test]
fn rejects_duplicate_group_member() {}

#[test]
fn rejects_nested_group_and_self_reference() {}

#[test]
fn rejects_invalid_source_name() {}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```text
cargo test --test source_config_test
```

Expected: 新增测试因 source 类型、graph 和校验接口尚不存在而失败。

- [ ] **Step 3: 实现最小配置模型**

实现受限枚举和 source graph 校验：

```rust
enum SourceType {
    Proxy,
    Group,
}

struct SourceConfig {
    name: String,
    source_type: SourceType,
    base_url: Option<Url>,
    require_https: bool,
    members: Vec<String>,
}
```

首期 group 只允许引用 proxy；不允许嵌套 group。source name 只能包含 ASCII
字母、数字、`.`、`_` 和 `-`，且不能为空。

- [ ] **Step 4: 运行配置测试**

Run:

```text
cargo test --test source_config_test --test config_test
```

Expected: 配置和 source graph 测试通过，既有 Rustup 测试按新 schema 更新后通过。

- [ ] **Step 5: 同步示例和用户文档**

将 Rustup 示例改为 `rustup.sources`，明确 `type = "proxy"`、`type = "group"`、
`members` 和新路由；补充 Cargo 配置示例。README 只描述已实现的 Rustup/Cargo
能力，未实现生态标记为后续能力。

- [ ] **Step 6: 执行格式和完整检查**

Run:

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

---

### Task 2: 公共来源访问与命名路由

**Files:**
- Create: `src/sources/mod.rs`
- Create: `src/sources/pool.rs`
- Create: `src/sources/fetcher.rs`
- Modify: `src/http.rs`
- Modify: `src/lib.rs`
- Create: `tests/sources_test.rs`
- Create: `tests/route_test.rs`

**Interfaces:**
- Consumes `SourceGraph` from Task 1.
- Produces `SourcePool::proxy_members(entry_name)`.
- Produces `SourcePool::fetch(entry_name, request_key, operation)`.
- Produces路由分发：解析 ecosystem/name，找到适配器和 source entry。

- [ ] **Step 1: 写失败测试**

覆盖：

```rust
#[test]
fn group_resolves_members_in_declared_order() {}

#[test]
fn proxy_entry_does_not_fallback_to_sibling_proxy() {}

#[tokio::test]
async fn group_falls_back_after_upstream_failure() {}

#[tokio::test]
async fn route_uses_ecosystem_and_source_name() {}

#[tokio::test]
async fn unknown_ecosystem_or_source_returns_not_found() {}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```text
cargo test --test sources_test --test route_test
```

Expected: 因公共来源池和新路由尚未存在而失败。

- [ ] **Step 3: 实现 SourcePool 和 HttpFetcher**

SourcePool 只执行 group 已解析的 proxy 顺序；proxy entry 只执行自身。HttpFetcher
禁止自动跟随未验证重定向，只接收配置来源生成的 URL，统一检查成功状态并分类
上游错误。

- [ ] **Step 4: 实现 AdapterRegistry 路由分发**

路由使用：

```text
/mirror/{ecosystem}/{name}/{*path}
```

未知生态或 source name 返回 404。适配器负责剩余 path 的解析，路由层不处理生态
协议细节。

- [ ] **Step 5: 运行来源和路由测试**

Run:

```text
cargo test --test sources_test --test route_test
```

Expected: 新来源池和命名路由测试通过。

- [ ] **Step 6: 执行完整检查**

Run:

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

---

### Task 3: Rustup 命名入口迁移

**Files:**
- Modify: `src/rustup/manifest.rs`
- Modify: `src/service.rs`
- Modify: `src/storage.rs`
- Modify: `src/http.rs`
- Modify: `tests/http_test.rs`
- Modify: `tests/manifest_test.rs`
- Modify: `tests/service_test.rs`
- Modify: `docs/architecture/rustup-mvp.md`
- Modify: `README.md`

**Interfaces:**
- Consumes Task 1 的 Rustup source graph 和 Task 2 的 named entry routing。
- Produces Rustup canonical root `/mirror/rustup/{name}`。
- Rustup manifest 重写必须使用当前请求入口的生态/name 根地址。

- [ ] **Step 1: 写失败测试**

覆盖：

```rust
#[tokio::test]
async fn rustup_group_route_serves_manifest() {}

#[tokio::test]
async fn rustup_proxy_route_serves_only_its_proxy() {}

#[test]
fn rewritten_manifest_uses_named_rustup_root() {}

#[tokio::test]
async fn legacy_rustup_route_returns_not_found() {}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```text
cargo test --test http_test --test manifest_test --test service_test
```

Expected: 新命名入口未实现，旧路由测试需要按破坏性迁移规则调整。

- [ ] **Step 3: 实现 Rustup 入口绑定**

将 manifest 重写函数接收当前入口根地址；group 请求只通过 group 成员解析来源。
Rustup 的 snapshot 和 artifact 文件继续由文件系统保存，不引入数据库。

- [ ] **Step 4: 删除旧路由**

移除 `/mirror/rustup/dist/**` 注册和对应测试，新增命名入口路由测试。更新
`RUSTUP_DIST_SERVER` 文档示例为：

```text
https://stacktory.example.internal/mirror/rustup/group
```

- [ ] **Step 5: 运行 Rustup 回归**

Run:

```text
cargo test --test http_test --test manifest_test --test service_test
```

Expected: Rustup 命名入口、manifest 重写、来源回退、快照和缓存测试通过。

- [ ] **Step 6: 执行完整检查**

Run:

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

---

### Task 4: Cargo Sparse Index 与 crate 只读代理

**Files:**
- Create: `src/cargo/mod.rs`
- Create: `src/cargo/path.rs`
- Create: `src/cargo/index.rs`
- Create: `src/cargo/service.rs`
- Modify: `src/http.rs`
- Modify: `src/lib.rs`
- Modify: `tests/config_test.rs`
- Create: `tests/cargo_path_test.rs`
- Create: `tests/cargo_index_test.rs`
- Create: `tests/cargo_service_test.rs`
- Modify: `README.md`
- Create: `docs/architecture/cargo-mvp.md`

**Interfaces:**
- Consumes Task 1 的 Cargo source graph、Task 2 的 SourcePool 和公共缓存/完整性能力。
- Produces Cargo named entry routes:

```text
/mirror/cargo/{name}/config.json
/mirror/cargo/{name}/index/**
/mirror/cargo/{name}/crates/**
```

- `CargoAdapter` 负责 crate 名称规范化、Sparse Index 路径、index 记录解析、
  下载地址重写和 crate 摘要校验。

- [ ] **Step 1: 写失败测试**

覆盖：

```rust
#[test]
fn computes_sparse_index_path_for_short_crate_name() {}

#[test]
fn computes_sparse_index_path_for_two_character_crate_name() {}

#[test]
fn computes_sparse_index_path_for_long_crate_name() {}

#[test]
fn rejects_invalid_crate_path() {}

#[test]
fn rewrites_crate_download_url_to_stacktory_entry() {}

#[tokio::test]
async fn cargo_group_serves_index_and_crate_with_fallback() {}

#[tokio::test]
async fn cargo_crate_hash_mismatch_does_not_publish_cache() {}
```

- [ ] **Step 2: 运行测试确认失败**

Run:

```text
cargo test --test cargo_path_test --test cargo_index_test --test cargo_service_test
```

Expected: Cargo adapter、路径算法和服务接口尚不存在而失败。

- [ ] **Step 3: 实现 Cargo 路径和 index 处理**

实现 Cargo Sparse Index 的 crate 名称规范化和路径算法；解析 index JSON Lines，
只重写协议允许的 crate 下载地址，保留版本记录和 checksum。

- [ ] **Step 4: 实现 Cargo config.json**

返回与 Cargo Sparse Registry 兼容的配置，`dl` 指向当前命名入口的 crates 路径，
不暴露任何上游地址。index 请求和 crate 请求都通过 SourcePool。

- [ ] **Step 5: 实现 Cargo metadata/artifact 缓存**

使用文件系统缓存，缓存键包含 ecosystem、entry name、协议角色和 object key。
crate 有 checksum 时必须校验；校验失败不得发布缓存，并允许 group 继续尝试下
一个 proxy。

- [ ] **Step 6: 运行 Cargo 测试**

Run:

```text
cargo test --test cargo_path_test --test cargo_index_test --test cargo_service_test
```

Expected: Cargo Sparse Index、地址重写、来源回退、缓存和完整性测试通过。

- [ ] **Step 7: 补充 Cargo 文档并执行完整检查**

Run:

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

---

### Task 5: 后续生态适配器契约

**Files:**
- Modify: `docs/architecture/multi-ecosystem-readonly-proxy.md`
- Create: `docs/architecture/npm-upm-mvp.md`
- Create: `docs/architecture/pypi-mvp.md`
- Create: `docs/architecture/nuget-mvp.md`

**Interfaces:**
- Consumes公共 source/group、路由、缓存、完整性和审计契约。
- Produces每个后续生态的独立协议边界，不实现生产代码。

- [ ] **Step 1: 固定 npm/UPM 共享核心边界**

明确 npm metadata、scoped package、tarball、UPM registry 和 scope 的复用关系；
UPM 使用独立生态入口，不复制 npm 核心实现。

- [ ] **Step 2: 固定 PyPI Simple API 边界**

明确项目名规范化、HTML/JSON 表示、Accept 协商、文件链接和哈希校验。

- [ ] **Step 3: 固定 NuGet V3 restore 边界**

明确 service index、registration、package content 的适配职责；搜索、push、delete
和 API key 保持后续范围。

- [ ] **Step 4: 完成设计文档一致性检查**

确认所有后续生态都使用 `/mirror/{ecosystem}/{name}/...`，source/group 语义一致，
不重新引入 `default_source`、隐式 group 或数据库依赖。

---

## 完成门槛

- 配置模型、source graph、命名路由和 Rustup 迁移完成。
- Cargo Sparse Index 和 crate 下载完成并有本地 HTTP 集成测试。
- 旧 `/mirror/rustup/dist/**` 路由不再注册。
- 未实现生态不暴露假路由。
- 不引入数据库。
- 完整执行：

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

- npm、UPM、PyPI、NuGet 后续实现边界已形成独立架构文档，不与 Cargo 实现混杂。
