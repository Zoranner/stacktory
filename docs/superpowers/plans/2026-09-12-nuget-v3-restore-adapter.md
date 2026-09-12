# NuGet V3 Restore Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement read-only NuGet V3 service index, registration, and package content restore routes with source affinity and integrity-safe caching.

**Architecture:** Add a NuGet-specific adapter and service that own protocol parsing and relationship state. Reuse `SourcePool` for configured proxy URL access and group ordering, `AdapterRegistry` for route registration, `AuditSink` for structured events, and atomic file writes for cache publication.

**Tech Stack:** Rust 2024, Axum 0.8, Reqwest 0.12, Serde JSON/TOML, Tokio, SHA-256/SHA-512, local HTTP integration tests, dotnet CLI.

**Spec:** `docs/superpowers/specs/2026-09-12-nuget-v3-restore-adapter-design.md`

## Global Constraints

- NuGet proxy `base_url` must be an actual `.../v3/index.json`.
- Only service index, registration, and `.nupkg` restore routes are supported.
- Unknown service resources and search are never exposed as usable routes.
- Registration content URLs must be proven for the exact package ID/version.
- Package content is published only after available digest/content validation.
- Service index, registration, and package content caches remain separate and source-bound.
- Group sources never merge metadata or package version sets.
- No push, delete, unlist, API key, account, permission, symbol server, or write API.
- No worktree, branch, remote push, tag, release, or deployment action.

---

### Task 1: Service Index Contract And Resource Rewriting

**Files:**
- Create: `src/nuget/mod.rs`
- Create: `src/nuget/path.rs`
- Create: `src/nuget/service_index.rs`
- Create: `tests/nuget_service_index_test.rs`
- Modify: `src/lib.rs`
- Modify: `src/config.rs`
- Modify: `tests/config_test.rs`

**Produces:**
- Strict NuGet base URL validation.
- Service index parser that recognizes registration and package content resources.
- Local service index URL rewriting with search omitted.
- Entry-owned service-index cache and selected-proxy sidecar.

- [ ] Write failing unit and HTTP tests for strict `/v3/index.json`, resource discovery, local rewriting, unknown resource omission, and no upstream host leakage.
- [ ] Run the focused tests and confirm they fail because no NuGet implementation exists.
- [ ] Implement safe package/source path validators and service index resource models.
- [ ] Implement proxy fetch through the configured actual service-index URL and group selection without merging.
- [ ] Implement service index cache publication with selected-proxy/resource sidecar.
- [ ] Run focused NuGet tests, `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, related config tests, and `git diff --check`.
- [ ] Commit as `实现 NuGet service index 资源发现`.

### Task 2: Registration Relationship Parsing

**Files:**
- Create: `src/nuget/registration.rs`
- Modify: `src/nuget/mod.rs`
- Modify: `src/nuget/service_index.rs`
- Create: `tests/nuget_registration_test.rs`

**Produces:**
- Registration index/page/leaf read-only routes.
- Exact package ID/version relationship validation.
- Rewritten registration links and package content bindings.
- Entry-owned registration cache and relationship sidecars.

- [ ] Write failing tests for index/page/leaf requests, page and leaf link rewriting, package identity checks, and rejection of external/unbound `packageContent`.
- [ ] Run focused registration tests to confirm the missing adapter behavior.
- [ ] Implement registration path parsing and discovered-resource URL resolution.
- [ ] Implement registration JSON validation without interpreting NuGet version ordering.
- [ ] Persist validated leaf bindings containing selected proxy, package ID/version, upstream content URL, and digest metadata.
- [ ] Run focused registration tests and all existing tests affected by routing/configuration.
- [ ] Run formatting, clippy, and diff checks.
- [ ] Commit as `解析 NuGet registration 包关系`.

### Task 3: Package Content Validation And Cache

**Files:**
- Create: `src/nuget/package.rs`
- Modify: `src/nuget/registration.rs`
- Create: `tests/nuget_package_test.rs`

**Produces:**
- Exact `.nupkg` route validation.
- Registration-bound package download.
- SHA-512/SHA-256 validation where declared and local SHA-256 cache metadata otherwise.
- Temporary-file download and atomic cache publication.

- [ ] Write failing tests for bound package download, ID/version filename validation, digest success, digest failure, no bad cache publication, and cache hit.
- [ ] Run focused package tests to confirm expected failures.
- [ ] Implement bound package lookup from published registration leaf state.
- [ ] Implement configured-source URL validation for package content and fetch using artifact timeout.
- [ ] Implement digest decoding and comparison, temporary file handling, atomic publication, and cache verification.
- [ ] Run focused package tests, related registration/service-index tests, formatting, clippy, and diff checks.
- [ ] Commit as `校验并缓存 NuGet package content`.

### Task 4: Group Fallback, Affinity, And Audit

**Files:**
- Modify: `src/nuget/service_index.rs`
- Modify: `src/nuget/registration.rs`
- Modify: `src/nuget/package.rs`
- Modify: `src/nuget/mod.rs`
- Create: `tests/nuget_group_test.rs`

**Produces:**
- Selected-proxy affinity across service index, registration, and package content.
- Retryable/404 group fallback with no source merge.
- Audit events for cache, source, fallback, relationship, and integrity outcomes.

- [ ] Write failing local HTTP tests for first-source metadata selection, fallback on unavailable/invalid/integrity failure, direct proxy versus group cache isolation, and no merged registration/version sets.
- [ ] Run focused group tests and confirm failures.
- [ ] Reorder fallback attempts from valid sidecar selected proxy while preserving declared group order.
- [ ] Update selected-proxy sidecars only after valid metadata/content publication.
- [ ] Add detailed audit records for all NuGet object roles and failure categories.
- [ ] Run focused group tests plus all current Rust tests, formatting, clippy, and diff checks.
- [ ] Commit as `绑定 NuGet 来源并实现 group 回退`.

### Task 5: Main Registration, Configuration, And Documentation

**Files:**
- Modify: `src/main.rs`
- Modify: `src/lib.rs`
- Modify: `config.toml`
- Modify: `deploy/app/config.example.toml`
- Modify: `README.md`
- Modify: `docs/architecture/nuget-mvp.md`
- Modify: `tests/route_test.rs`
- Modify: `tests/config_test.rs`

**Produces:**
- Main program registration for configured NuGet sources.
- Actual `https://api.nuget.org/v3/index.json` example.
- Documented supported and unsupported routes.

- [ ] Write failing tests for NuGet adapter registry registration and unsupported method/route handling.
- [ ] Run focused route/config tests to confirm the missing registration.
- [ ] Register `NugetAdapter` in `main.rs` and export the adapter constructor.
- [ ] Update root/deployment configuration and documentation with the actual service-index URL and Stacktory restore endpoint.
- [ ] Run route/config/documentation-adjacent tests, formatting, clippy, and diff checks.
- [ ] Commit as `注册 NuGet restore 适配器`.

### Task 6: Local HTTP Restore Verification And Real dotnet Restore

**Files:**
- Modify: `tests/nuget_service_index_test.rs`
- Modify: `tests/nuget_registration_test.rs`
- Modify: `tests/nuget_package_test.rs`
- Modify: `README.md`

- [ ] Run the complete local upstream test suite covering service index, registration, package content, path traversal, unknown resources, redirect handling, fallback, cache isolation, and audit output.
- [ ] Run `cargo fmt --all`.
- [ ] Run `cargo test --all-targets --all-features --locked`.
- [ ] Run `cargo clippy --all-targets --all-features -- -D warnings`.
- [ ] Run `git diff --check`.
- [ ] Start the service from the repository root using a temporary NuGet configuration and local/real upstream settings as appropriate.
- [ ] Run a separate `dotnet restore` against `/mirror/nuget/{entry}/v3/index.json`, record whether the real client completes, and keep client verification separate from local protocol tests.
- [ ] Commit final verification/documentation changes as `验证 NuGet restore 客户端链路`.
