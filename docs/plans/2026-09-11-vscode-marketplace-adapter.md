# VS Code Marketplace Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the VS Code extension side panel search and install Microsoft Marketplace extensions through Stacktory.

**Architecture:** Replace the Open VSX-specific `vscode` adapter with a Microsoft Gallery adapter. Extend the generic registry and source client to accept bounded POST JSON requests, rewrite Gallery asset URLs to Stacktory descriptors, and cache assets by actual proxy plus upstream URL identity.

**Tech Stack:** Rust 2024, Axum 0.8, Reqwest 0.12, Serde JSON/TOML, Tokio, VS Code 1.136.1.

**Spec:** `docs/specs/2026-09-11-vscode-marketplace-adapter-design.md`

## Global Constraints

- Preserve Rustup and Cargo GET behavior and their current source security boundaries.
- Remove, do not preserve, the Open VSX `/api/...` VS Code protocol.
- Only Microsoft Gallery `extensionquery` and query-published assets are in scope.
- Query bodies are limited to 1 MiB.
- Keep metadata entry-owned and binary assets source-owned.
- Do not modify the user's normal VS Code runtime, settings, or extensions.
- Use `cargo fmt --all`, `cargo test --all-targets --all-features --locked`, and `cargo clippy --all-targets --all-features -- -D warnings`.

---

### Task 1: Generalize Adapter And Source Requests

**Files:**
- Modify: `src/http.rs`
- Modify: `src/sources/registry.rs`
- Modify: `src/sources/fetcher.rs`
- Modify: `src/sources/pool.rs`
- Modify: `src/cargo/service.rs`
- Test: `tests/route_test.rs`
- Test: `tests/sources_test.rs`

**Interfaces:**
- Produces `AdapterRequest { method, path, body }` for registered adapters.
- Produces bounded GET and POST JSON requests through `SourcePool`.
- Cargo continues to handle only GET requests.

- [ ] **Step 1: Write failing route tests**

Add a test that sends `POST /mirror/demo/internal/extensionquery` with `{"filters":[]}` and asserts the adapter receives `Method::POST` and the exact body. Add a test that sends a body larger than 1 MiB and asserts `413 Payload Too Large`.

- [ ] **Step 2: Run route tests to verify failure**

Run:

```text
cargo test --test route_test --locked
```

Expected: the POST route returns `405` because the current router only registers `get`.

- [ ] **Step 3: Add the generic request types**

Define:

```rust
pub struct AdapterRequest {
    pub method: http::Method,
    pub path: String,
    pub body: Vec<u8>,
}
```

Change `Adapter::handle` to receive `AdapterRequest`. In `http.rs`, register `any(named_source)`, bound the Axum body to 1 MiB, and map body overflow to `413`.

Define a source request mode that distinguishes GET metadata, GET artifact, and POST JSON metadata. Keep POST JSON request headers fixed in the source boundary instead of forwarding arbitrary client headers.

- [ ] **Step 4: Preserve Cargo behavior**

Make `CargoAdapter` return `405 Method Not Allowed` for every method except GET. Update Cargo tests to construct `AdapterRequest` with `Method::GET`.

- [ ] **Step 5: Run focused tests**

Run:

```text
cargo test --test route_test --test sources_test --test cargo_service_test --locked
```

Expected: POST route forwarding works, oversized bodies return `413`, and Cargo tests remain green.

### Task 2: Replace Open VSX With Marketplace Query And Asset Model

**Files:**
- Delete: `src/vscode/path.rs`
- Rewrite: `src/vscode/service.rs`
- Modify: `src/vscode/mod.rs`
- Test: `tests/vscode_protocol_test.rs`

**Interfaces:**
- Consumes `AdapterRequest` and `SourcePool`.
- Produces `POST extensionquery` and `GET asset/{asset-id}`.
- Persists `marketplace/{query-id}.json` and `marketplace/assets/{asset-id}.toml`.

- [ ] **Step 1: Write failing marketplace query tests**

Use a local Axum upstream at `/extensionquery`. Assert that Stacktory sends POST JSON with:

```text
Accept: application/json;api-version=3.0-preview.1
Content-Type: application/json
X-Market-Client-Id: VSCode
```

Return a minimal Gallery response with one extension, one version, and one `Microsoft.VisualStudio.Services.VSIXPackage` file. Assert its `source` becomes `/mirror/vscode/group/asset/{64-hex-id}` and that a descriptor contains the original URL and selected source.

- [ ] **Step 2: Run the query test to verify failure**

Run:

```text
cargo test --test vscode_protocol_test marketplace_query_rewrites_assets --locked
```

Expected: failure because the current adapter only parses `/api/...` paths.

- [ ] **Step 3: Implement Gallery query parsing and rewrite**

Replace `VscodePath` with a route parser that accepts exactly:

```text
POST extensionquery
GET asset/{64-hex-id}
```

Hash the exact query request body for `query-id`. Parse the Gallery response recursively only through:

```text
results[].extensions[].versions[].files[]
```

Require `assetType` and a safe HTTP(S) `source`, replace each `source`, and atomically write the rewritten response plus per-asset TOML descriptors.

- [ ] **Step 4: Implement asset resolution and source-owned cache**

Load only an existing descriptor for the requested asset ID. Fetch its original URL through the descriptor's selected proxy using the artifact timeout. Cache body and content type under:

```text
data_dir/vscode/artifacts/{proxy}/objects/marketplace/{asset-id}
```

Return cached or fetched bytes with the stored content type. Do not fetch arbitrary client URLs or unbound asset IDs.

- [ ] **Step 5: Add security and reuse tests**

Add tests for invalid query JSON, malformed Gallery files, unsafe asset URLs, missing descriptors, direct/group source cache reuse, and Cargo POST rejection.

- [ ] **Step 6: Run VS Code protocol tests**

Run:

```text
cargo test --test vscode_protocol_test --locked
```

Expected: Gallery query, asset rewrite, descriptor binding, cache reuse, and invalid path behavior pass.

### Task 3: Configure Microsoft Marketplace And Remove Open VSX Contracts

**Files:**
- Modify: `config.toml`
- Modify: `deploy/app/config.example.toml`
- Modify: `README.md`
- Rewrite: `docs/architecture/vscode-mvp.md`
- Modify: `tests/config_test.rs`

**Interfaces:**
- Produces `[vscode]` source graph with `official` and `group`.
- Documents `extensions.gallery.serviceUrl`.

- [ ] **Step 1: Write failing configuration test**

Add an assertion that the deployment configuration contains a `vscode` source graph with:

```text
official -> https://marketplace.visualstudio.com/_apis/public/gallery
group -> ["official"]
```

- [ ] **Step 2: Run configuration test to verify failure**

Run:

```text
cargo test --test config_test deployment_config_matches_the_runtime_schema --locked
```

Expected: failure because no `[vscode]` section exists.

- [ ] **Step 3: Add configuration and replace documentation**

Add the `vscode` graph to root and deployment configuration. Remove Open VSX routes and VSIX-install instructions from README. Document side-panel configuration only through:

```json
{
  "extensions.gallery.serviceUrl": "http://127.0.0.1:18761/mirror/vscode/group"
}
```

- [ ] **Step 4: Run configuration and documentation-adjacent tests**

Run:

```text
cargo test --test config_test --test vscode_protocol_test --locked
```

Expected: deployment config parses and the Marketplace adapter tests pass.

### Task 4: Real Microsoft Gallery And VS Code Side-Panel Verification

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes the running Stacktory service and local VS Code 1.137.0.
- Produces an isolated copy of the VS Code runtime with Stacktory product configuration.

- [ ] **Step 1: Verify real Gallery query through Stacktory**

Send the VS Code Gallery query JSON for `redhat.vscode-yaml` to:

```text
POST http://127.0.0.1:18761/mirror/vscode/group/extensionquery
```

Assert response status `200`, extension `redhat.vscode-yaml`, and rewritten `source` values under Stacktory.

- [ ] **Step 2: Verify a rewritten VSIX asset**

Extract the `Microsoft.VisualStudio.Services.VSIXPackage` rewritten asset URL from the query response, request it, and assert `200` plus a nonempty body.

- [ ] **Step 3: Start isolated VS Code**

Copy the installed VS Code runtime to a temporary directory. In the copied
`resources/app/product.json`, replace only `extensionsGallery.serviceUrl` with:

```json
{
  "extensionsGallery": {
    "serviceUrl": "http://127.0.0.1:18761/mirror/vscode/group"
  }
}
```

Start:

```text
<temporary-vscode>/Code.exe --user-data-dir <temporary-user-data> --extensions-dir <temporary-extensions>
```

Open the Extensions side panel, search `YAML`, select `YAML`, and install it. Confirm Stacktory audit records one Marketplace query and one asset request.

- [ ] **Step 4: Run full Rust verification**

Run:

```text
cargo fmt --all
cargo test --all-targets --all-features --locked
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

Expected: all commands exit successfully.
