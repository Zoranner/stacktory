# npm Registry Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the read-only npm Registry adapter with metadata rewriting, bound tarball downloads, integrity validation, group fallback, and npm CLI compatibility.

**Architecture:** Add an npm-specific adapter under `src/npm/` and keep UPM unregistered and independent. The adapter will use the existing `SourcePool`, `AdapterRegistry`, audit sink, and atomic file writes, while storing npm metadata and artifact descriptors in source-aware filesystem paths. Metadata selects and persists one proxy; tarball requests use that binding first and only fall back to later group members after source failure or integrity failure.

**Tech Stack:** Rust 2024, Axum, Tokio, Reqwest, Serde JSON, TOML, SHA-256, local HTTP integration tests, npm CLI.

**Spec:** `docs/architecture/npm-upm-mvp.md`

## Global Constraints

- Implement only read-only npm metadata and tarball GET routes.
- Keep npm and UPM as separate adapters and ecosystems.
- Accept only `/mirror/npm/{name}/{package}`, `/mirror/npm/{name}/@scope%2Fpackage`, and `/mirror/npm/{name}/tarballs/{artifact-key}`.
- Reject invalid npm package names, unsafe encoded paths, traversal, arbitrary URLs, and unbound artifact keys.
- Preserve npm metadata fields needed by restore/install; rewrite only tarball URLs to the Stacktory tarballs route.
- Prefer `dist.integrity`, support `dist.shasum` when present, and do not claim upstream integrity verification when no upstream digest exists.
- Cache keys include ecosystem, entry, protocol variant, package, version, representation, and selected proxy.
- Do not implement publish, unpublish, dist-tag mutation, accounts, tokens, permissions, or write APIs.
- Run focused tests, `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and staged diff checks before every implementation commit.
- Do not create a worktree or branch and do not push.

---

### Task 1: Preserve Raw URI and Implement npm Path Parsing

**Files:**
- Modify: `src/sources/registry.rs`
- Modify: `src/http.rs`
- Create: `src/npm/mod.rs`
- Create: `src/npm/path.rs`
- Create: `tests/npm_path_test.rs`

**Interfaces:**
- `AdapterRequest` exposes the raw request path needed to distinguish `%2F` from a decoded slash.
- `NpmPackagePath::parse_metadata(raw_path: &str)` returns a validated package object key.
- `NpmArtifactPath::parse(raw_path: &str)` returns a validated artifact key.

- [ ] **Step 1: Add failing parser tests**

Cover:

```rust
assert_eq!(parse_metadata("lodash").unwrap().package, "lodash");
assert_eq!(parse_metadata("@scope%2Fpkg").unwrap().package, "@scope/pkg");
assert!(parse_metadata("@scope/pkg").is_err());
assert!(parse_metadata("../pkg").is_err());
assert!(parse_metadata("@scope%252Fpkg").is_err());
assert!(parse_metadata("a//b").is_err());
assert!(parse_artifact("a".repeat(64).as_str()).is_ok());
assert!(parse_artifact("https://registry.example/pkg.tgz").is_err());
```

- [ ] **Step 2: Run the focused parser tests and confirm the expected missing-interface failure**

Run:

```text
cargo test --test npm_path_test
```

Expected: compilation fails because the npm path types and raw-path field do not exist.

- [ ] **Step 3: Implement raw path forwarding and strict npm path parsing**

Keep the raw URI path available at the generic route boundary. Permit exactly one encoded slash between a valid `@scope` and package segment for metadata. Reject all other percent escapes, decoded slash forms, traversal segments, empty segments, control characters, query/fragment material, backslashes, and invalid npm names.

- [ ] **Step 4: Run the parser tests and formatting**

Run:

```text
cargo test --test npm_path_test
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Inspect and commit**

```text
git diff --check
git add src/sources/registry.rs src/http.rs src/npm/mod.rs src/npm/path.rs tests/npm_path_test.rs
git diff --cached --check
git commit -m "实现 npm Registry 路径解析"
```

### Task 2: Parse npm Metadata and Rewrite Tarball URLs

**Files:**
- Create: `src/npm/metadata.rs`
- Modify: `src/npm/mod.rs`
- Create: `tests/npm_metadata_test.rs`

**Interfaces:**
- `parse_metadata(body: &[u8], package: &str) -> Result<NpmMetadata, NpmError>`.
- `rewrite_metadata(metadata: &NpmMetadata, root_url: &Url, entry: &str, selected_proxy: &str) -> Result<RewrittenMetadata, NpmError>`.
- `RewrittenMetadata` contains rewritten JSON bytes and descriptors for every version with a valid tarball.

- [ ] **Step 1: Add failing metadata tests**

Assert that:

```rust
let rewritten = rewrite_metadata(...).unwrap();
assert_eq!(rewritten.body["name"], "@scope/pkg");
assert_eq!(
    rewritten.body["versions"]["1.0.0"]["dist"]["tarball"],
    "http://127.0.0.1:18761/mirror/npm/group/tarballs/<artifact-key>"
);
assert_eq!(rewritten.body["versions"]["1.0.0"]["dist"]["integrity"], original_integrity);
assert_eq!(rewritten.body["versions"]["1.0.0"]["dist"]["shasum"], original_shasum);
assert!(rewritten.body["versions"]["1.0.0"]["dependencies"].is_object());
assert_eq!(rewritten.descriptors[0].package, "@scope/pkg");
assert_eq!(rewritten.descriptors[0].selected_proxy, "official");
```

Reject metadata whose top-level package name does not match the requested package, whose versions are not objects, or whose `dist.tarball` is missing/unsafe.

- [ ] **Step 2: Run the focused metadata tests and verify the expected failure**

Run:

```text
cargo test --test npm_metadata_test
```

- [ ] **Step 3: Implement loss-minimizing JSON parsing and URL rewriting**

Use `serde_json::Value` to preserve unknown fields. Validate package/version keys, parse semver-compatible version strings only as needed for artifact identity, preserve the original `dist` fields, and replace only `dist.tarball`. Generate a deterministic artifact key from ecosystem, entry, protocol variant, package, version, representation, selected proxy, and the validated upstream tarball identity.

- [ ] **Step 4: Run tests and static checks**

Run:

```text
cargo test --test npm_metadata_test
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Inspect and commit**

```text
git diff --check
git add src/npm/mod.rs src/npm/metadata.rs tests/npm_metadata_test.rs
git diff --cached --check
git commit -m "重写 npm metadata 制品地址"
```

### Task 3: Add npm Metadata and Tarball Cache with Integrity Validation

**Files:**
- Create: `src/npm/storage.rs`
- Modify: `src/npm/service.rs`
- Create: `tests/npm_cache_test.rs`

**Interfaces:**
- `NpmStorage::read_metadata(...)`.
- `NpmStorage::publish_metadata(...)`.
- `NpmStorage::read_descriptor(...)`.
- `NpmStorage::publish_descriptor(...)`.
- `NpmStorage::read_verified_artifact(...)`.
- `NpmStorage::publish_verified_artifact(...)`.

- [ ] **Step 1: Add failing cache and integrity tests**

Cover:

```rust
assert!(cache_hit_after_sha512_integrity_match());
assert!(cache_hit_after_sha1_shasum_match());
assert!(bad_integrity_is_not_published());
assert!(missing_digest_records_local_sha256_without_upstream_proof());
assert!(npm_cache_paths_include_entry_package_version_representation_and_proxy());
```

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```text
cargo test --test npm_cache_test
```

- [ ] **Step 3: Implement source-bound filesystem storage**

Store metadata and descriptors under `data_dir/npm/metadata/{entry}/...`; store artifacts under `data_dir/npm/artifacts/{selected_proxy}/objects/...`. Ensure the logical cache identity contains ecosystem, entry, protocol variant, package, version, representation, and selected proxy even when the physical path also uses a digest. Use atomic writes and delete invalid cached artifacts before refetching.

- [ ] **Step 4: Implement integrity checking**

Decode supported SRI integrity values, preferring `sha512`/the strongest available `dist.integrity` value, then use `dist.shasum` as SHA-1. Always compute a local SHA-256 for audit/cache metadata. A missing upstream digest is a successful fetch without an upstream integrity-proof claim.

- [ ] **Step 5: Run tests and static checks**

Run:

```text
cargo test --test npm_cache_test
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Inspect and commit**

```text
git diff --check
git add src/npm/service.rs src/npm/storage.rs tests/npm_cache_test.rs
git diff --cached --check
git commit -m "校验并缓存 npm tarball"
```

### Task 4: Implement Metadata Selection, Group Fallback, and Source Binding

**Files:**
- Modify: `src/npm/service.rs`
- Modify: `src/npm/metadata.rs`
- Create: `tests/npm_service_test.rs`

**Interfaces:**
- `NpmService::metadata(entry, package, pool)`.
- `NpmService::tarball(entry, artifact_key, pool)`.
- Stored descriptors bind package, version, representation, selected proxy, and validated upstream tarball URL.

- [ ] **Step 1: Add failing local HTTP integration tests**

Cover ordinary packages, scoped packages, metadata rewrite, tarball retrieval, selected proxy reuse, group fallback after source failure, group fallback after digest failure, no fallback for terminal client errors, unbound artifact keys, and no cache publication for invalid content.

- [ ] **Step 2: Run the npm service tests and verify failure**

Run:

```text
cargo test --test npm_service_test
```

- [ ] **Step 3: Implement metadata fetch and persistence**

Fetch `package` from the entry through the existing source pool. On success, validate and rewrite the metadata, persist the metadata plus descriptors bound to the response source, and return `application/json`. Do not merge version sets from multiple proxies.

- [ ] **Step 4: Implement bound tarball retrieval**

Load the descriptor from the artifact key, verify it belongs to the requested entry and an allowed proxy member, then try its selected proxy first. Fetch only the stored validated URL, verify integrity, and publish only verified bytes.

- [ ] **Step 5: Implement fallback semantics**

For a group, try later declared proxies only after unavailable upstream responses, metadata parse failures, invalid stored tarball URLs, or integrity failures. Keep the artifact key and descriptor identity bound to the metadata-selected proxy; for each fallback proxy, fetch package metadata again, locate the same version, validate its tarball and digest, and cache the verified bytes under the actual fallback proxy while serving the original internal artifact route. Preserve terminal upstream 4xx behavior.

- [ ] **Step 6: Add npm adapter error mapping and audit events**

Map invalid/unbound paths and missing metadata versions to `404`, terminal upstream statuses to their status where appropriate, source or integrity failures to `502`, and local storage failures to `500`. Record metadata read/publish, artifact hit/publish, fallback, and failure events with ecosystem, entry, object key, selected proxy, representation, and failure category.

- [ ] **Step 7: Run focused tests and static checks**

Run:

```text
cargo test --test npm_service_test
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 8: Inspect and commit**

```text
git diff --check
git add src/npm/metadata.rs src/npm/service.rs tests/npm_service_test.rs
git diff --cached --check
git commit -m "绑定 npm 来源并支持 group 回退"
```

### Task 5: Register npm in the Main Program

**Files:**
- Modify: `src/main.rs`
- Modify: `src/lib.rs`
- Modify: `tests/config_test.rs`
- Modify: `tests/http_test.rs`

- [ ] **Step 1: Add failing registration tests**

Assert that configured npm sources register `/mirror/npm/{name}/...`, while UPM remains unregistered and `/mirror/upm/...` remains `404`.

- [ ] **Step 2: Run the focused registration tests and verify failure**

Run:

```text
cargo test --test config_test --test http_test
```

- [ ] **Step 3: Register `NpmAdapter` only when `config.npm` is present**

Construct `SourcePool::new("npm", npm.sources)` and `NpmAdapter::new(config.root_url, config.data_dir)`, then register them with `AdapterRegistry` before building the router. Do not register UPM.

- [ ] **Step 4: Run focused tests, formatting, and Clippy**

Run:

```text
cargo test --test config_test --test http_test
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Inspect and commit**

```text
git diff --check
git add src/main.rs src/lib.rs tests/config_test.rs tests/http_test.rs
git diff --cached --check
git commit -m "注册 npm Registry 适配器"
```

### Task 6: Update User Documentation and Configuration Notes

**Files:**
- Modify: `README.md`
- Modify: `deploy/app/config.example.toml`
- Modify: `docs/architecture/npm-upm-mvp.md`

- [ ] **Step 1: Add documentation assertions or static checks**

Verify the README documents all three npm route forms, scoped package encoding, source binding, integrity limits, read-only scope, and explicitly says UPM is not registered.

- [ ] **Step 2: Update documentation**

Document npm client configuration, ordinary and scoped package examples, cache/source behavior, fallback conditions, and the distinction between missing digest and verified upstream integrity. Keep configuration examples aligned with the existing npm source samples.

- [ ] **Step 3: Run documentation checks and focused regression tests**

Run:

```text
rg -n 'mirror/npm|%2F|selected_proxy|integrity|UPM|publish' README.md docs/architecture/npm-upm-mvp.md deploy/app/config.example.toml
cargo test --test npm_path_test --test npm_metadata_test --test npm_cache_test --test npm_service_test
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 4: Inspect and commit**

```text
git diff --check
git add README.md deploy/app/config.example.toml docs/architecture/npm-upm-mvp.md
git diff --cached --check
git commit -m "补充 npm Registry 使用文档"
```

### Task 7: Run Local HTTP Protocol Tests and Real npm CLI Install

**Files:**
- Modify: `tests/npm_service_test.rs`
- Create: `tests/manual/npm-registry-smoke.ps1`
- Modify: `README.md`

- [ ] **Step 1: Add a complete local upstream protocol fixture**

The fixture must serve ordinary metadata, scoped metadata at the encoded route, tarballs with valid and invalid integrity, and two sources whose counters prove selected-proxy reuse and ordered fallback.

- [ ] **Step 2: Run the complete local protocol suite**

Run:

```text
cargo test --test npm_path_test --test npm_metadata_test --test npm_cache_test --test npm_service_test
```

Expected: all npm protocol behaviors pass against local HTTP upstreams, including cache isolation and UPM non-registration.

- [ ] **Step 3: Build and start Stacktory locally**

Run from the repository root:

```text
cargo build --locked
```

Start the service with a temporary npm-enabled config and a temporary data directory, retaining the process for CLI verification and stopping only the process started by this task.

- [ ] **Step 4: Run npm CLI install against the local Stacktory registry**

Use a temporary npm project and registry configuration:

```text
npm init -y
npm install --registry http://127.0.0.1:<port>/mirror/npm/<entry> <fixture-package>@<version>
```

Verify the package is installed, the requested metadata used an internal tarball URL, and the upstream counters show the selected proxy was reused.

- [ ] **Step 5: Record the real CLI boundary**

Document the exact command and result. If the local npm executable or runtime cannot complete, report that as an environment verification gap rather than a code success.

- [ ] **Step 6: Run final repository checks and inspect status**

Run:

```text
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
git status --short
git diff --check
```

- [ ] **Step 7: Inspect and commit the CLI integration material**

```text
git diff --check
git add tests/npm_service_test.rs tests/manual/npm-registry-smoke.ps1 README.md
git diff --cached --check
git commit -m "完成 npm CLI 联调验证"
```
