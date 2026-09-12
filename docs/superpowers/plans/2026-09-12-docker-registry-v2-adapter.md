# Docker Registry V2 Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a read-only Docker Registry HTTP API V2 adapter exposed through `/mirror/docker/{name}/v2/...`.

**Architecture:** The Docker adapter owns Registry V2 path parsing, manifest semantics, digest validation, metadata bindings, and Docker response headers. It reuses the existing source graph and extends the shared HTTP/source layer only where request headers and response metadata are required. Entry-scoped metadata binds a repository/reference to the validated `selected_proxy`; verified content is shared by actual proxy, repository, object type, and digest so group and direct-proxy entries can reuse the same content.

**Tech Stack:** Rust 2024, Axum 0.8, Reqwest 0.12, Serde JSON, SHA-256, Tokio, existing `SourcePool`, `AuditSink`, and atomic file writes.

**Spec:** `docs/architecture/multi-ecosystem-readonly-proxy.md`

## Global Constraints

- Stacktory's public Docker path is `/mirror/docker/{name}/v2/...`; do not add `/mirror/docker/v2/...`.
- Upstream requests use `/v2/{repository}/manifests/{reference}` and `/v2/{repository}/blobs/{digest}`.
- Implement read-only pull behavior only; do not implement push, delete, catalog, account, signature, or build APIs.
- Do not expose upstream Registry hosts or arbitrary external URLs to Docker clients.
- A digest must be verified before an artifact is atomically written to cache.
- A group selects members in declared order and never merges manifests or tag lists from different proxies.
- Existing unrelated npm/NuGet changes must remain untouched; do not modify Rustup, Cargo, VS Code, deployment, remote branches, tags, or releases.
- Every stage ends with focused tests, `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `git diff --check`, staged diff review, and one Docker-scoped commit.

### Task 1: Registry V2 Route and Request Model

**Files:**
- Create: `src/docker/mod.rs`
- Create: `src/docker/path.rs`
- Test: `tests/docker_path_test.rs`

**Interfaces:**
- Produces `DockerAdapter`, `DockerService`, `DockerRequest`, `DockerPathError`, and parsers for `v2/`, manifest, and blob paths.
- Consumes `SourcePool`, `Adapter`, and `AdapterRequest`.

- [ ] **Step 1: Write failing path tests**

Test that `v2/` is a probe, `v2/library/alpine/manifests/latest` parses as a manifest request, `v2/library/alpine/manifests/sha256:<64-hex>` parses as a digest reference, and `v2/.../blobs/sha256:<64-hex>` parses as a blob request. Reject `/mirror/docker/v2/...` as an adapter path, V1 paths, unsafe repository segments, empty references, and non-SHA256 digests.

- [ ] **Step 2: Run `cargo test --test docker_path_test` and verify the missing-module failure**

- [ ] **Step 3: Implement only the path/request model and GET-only adapter shell**

The adapter must accept only `v2/`, `v2/{repository}/manifests/{reference}`, and `v2/{repository}/blobs/{digest}`. It must not construct or return any upstream URL in the client response.

- [ ] **Step 4: Run the focused path tests and route tests**

- [ ] **Step 5: Commit**

```text
git add src/docker tests/docker_path_test.rs
git diff --cached --check
git commit -m "实现 Docker Registry V2 路由模型"
```

### Task 2: Manifest Tag and Digest Handling

**Files:**
- Modify: `src/docker/mod.rs`
- Create: `tests/docker_manifest_test.rs`

**Interfaces:**
- Consumes `DockerRequest` and source-fetch response metadata.
- Produces manifest retrieval with `Accept` negotiation, `Docker-Content-Digest` handling, and tag metadata descriptors.

- [ ] **Step 1: Write failing local-upstream HTTP tests**

Cover `/v2/`, tag manifest, digest manifest, required Accept forwarding, response Content-Type, Docker-Content-Digest, and rejection of a manifest whose body does not match its declared digest.

- [ ] **Step 2: Run `cargo test --test docker_manifest_test` and verify the semantic failures**

- [ ] **Step 3: Implement manifest fetch and response construction**

Use upstream `/v2/{repository}/manifests/{reference}`. For tags, require a valid upstream digest header and persist the tag-to-digest metadata only after body verification. For digest references, verify against the requested digest. Return the validated body without rewriting it.

- [ ] **Step 4: Run the focused tests and inspect generated metadata**

- [ ] **Step 5: Commit**

```text
git add src/docker tests/docker_manifest_test.rs
git diff --cached --check
git commit -m "实现 Docker manifest 标签和摘要读取"
```

### Task 3: Image Index, Config, and Blob Handling

**Files:**
- Modify: `src/docker/mod.rs`
- Create: `tests/docker_artifact_test.rs`

**Interfaces:**
- Consumes validated manifests and descriptor metadata.
- Produces OCI image index, image config, and layer blob responses with distinct object types.

- [ ] **Step 1: Write failing local-upstream protocol tests**

Cover Docker manifest list, OCI image index, config blobs, layer blobs, and content types for each representation. Verify that a blob request is generated as `/v2/{repository}/blobs/{digest}`.

- [ ] **Step 2: Run `cargo test --test docker_artifact_test` and verify failures**

- [ ] **Step 3: Implement descriptor extraction and artifact serving**

Record config and layer descriptors from validated manifests and indexes. Resolve blob requests against those descriptors, fetch the upstream blob path, and return the corresponding media type and digest header.

- [ ] **Step 4: Run focused artifact tests**

- [ ] **Step 5: Commit**

```text
git add src/docker tests/docker_artifact_test.rs
git diff --cached --check
git commit -m "实现 Docker 镜像索引配置和层读取"
```

### Task 4: Digest Validation, Cache, and Selected Proxy Binding

**Files:**
- Modify: `src/docker/mod.rs`
- Modify: `src/storage.rs`
- Create: `tests/docker_cache_test.rs`

**Interfaces:**
- Consumes validated manifest/blob/config responses.
- Produces atomic verified content cache, entry-scoped metadata bindings, and cache hit/miss behavior.

- [ ] **Step 1: Write failing cache tests**

Verify wrong digest content is not returned as success and leaves no cache file; valid content is atomically cached; tag metadata records selected proxy; and the same digest reuses content cached through the actual selected proxy from both group and direct-proxy entries.

- [ ] **Step 2: Run `cargo test --test docker_cache_test` and verify failures**

- [ ] **Step 3: Implement Docker cache layout**

Store entry-scoped metadata under `data_dir/docker/metadata/{entry}/...`. Store verified content under `data_dir/docker/artifacts/{selected_proxy}/{repository}/{object_type}/{digest}`. Include ecosystem, repository, object type, digest, and selected proxy in the logical identity and validate cached bytes on every read.

- [ ] **Step 4: Run focused cache tests and inspect paths**

- [ ] **Step 5: Commit**

```text
git add src/docker src/storage.rs tests/docker_cache_test.rs
git diff --cached --check
git commit -m "增加 Docker 摘要校验和来源绑定缓存"
```

### Task 5: Group Fallback and Audit

**Files:**
- Modify: `src/docker/mod.rs`
- Create: `tests/docker_group_test.rs`

**Interfaces:**
- Consumes source graph member order and selected-proxy metadata.
- Produces ordered fallback, no-merge behavior, protocol/integrity-aware retries, and detailed audit events.

- [ ] **Step 1: Write failing group tests**

Use two local HTTP upstreams to verify declared member order, fallback after 404/upstream failure/digest mismatch, no fallback for a direct proxy, no merged manifest/tag set, and audit records containing selected proxy and failure category.

- [ ] **Step 2: Run `cargo test --test docker_group_test` and verify failures**

- [ ] **Step 3: Implement group selection and binding**

When a binding exists, try that proxy first. Only after retryable source failure or validation failure may a group try its next member. Persist the binding after complete protocol and digest validation. Never combine responses across members.

- [ ] **Step 4: Run focused group tests and inspect audit JSONL**

- [ ] **Step 5: Commit**

```text
git add src/docker tests/docker_group_test.rs
git diff --cached --check
git commit -m "实现 Docker group 回退和审计"
```

### Task 6: Main Program Registration

**Files:**
- Modify: `src/main.rs`
- Modify: `src/lib.rs`
- Test: `tests/docker_registration_test.rs`

**Interfaces:**
- Consumes `Config.docker`.
- Produces a registered `docker` adapter using the configured source graph, root URL, and data directory.

- [ ] **Step 1: Write failing registration test**

Verify a configured Docker source is registered and `/mirror/docker/{name}/v2/` reaches the adapter, while an absent Docker configuration does not add the route.

- [ ] **Step 2: Run the focused registration test and verify failure**

- [ ] **Step 3: Register `DockerAdapter` in `main` and expose the module**

- [ ] **Step 4: Run registration and full Rust tests**

- [ ] **Step 5: Commit**

```text
git add src/main.rs src/lib.rs tests/docker_registration_test.rs
git diff --cached --check
git commit -m "注册 Docker Registry V2 适配器"
```

### Task 7: Documentation and Real Client Verification

**Files:**
- Modify: `README.md`
- Modify: `docs/architecture/multi-ecosystem-readonly-proxy.md`
- Create or modify: `tests/manual/docker-pull.ts`

**Interfaces:**
- Documents the public Stacktory path and the supported read-only Docker protocol.
- Provides a reproducible local-upstream and public Docker/OCI client verification record.

- [ ] **Step 1: Write the verification script/checklist**

The script must start or target a local HTTP Registry V2-compatible upstream, exercise `/v2/`, tag/digest manifests, image index, config, blob, digest mismatch, group fallback, and cache reuse. It must not expose or hand an upstream URL to the Docker client.

- [ ] **Step 2: Run all focused Docker tests plus the complete test suite**

- [ ] **Step 3: Run `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `git diff --check`**

- [ ] **Step 4: Run the public Docker/OCI client pull against Stacktory**

Record whether Docker CLI or another public OCI client is available. If unavailable, report that boundary instead of claiming real-client success.

- [ ] **Step 5: Commit documentation and verification artifacts**

```text
git add README.md docs/architecture/multi-ecosystem-readonly-proxy.md tests/manual/docker-pull.ts
git diff --cached --check
git commit -m "补充 Docker Registry V2 使用和验证"
```
