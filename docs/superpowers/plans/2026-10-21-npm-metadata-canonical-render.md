# npm/UPM Metadata Canonical Render Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make npm/UPM metadata cache independent of `root_url`, and make stale or deleted upstream metadata refresh its own cache without touching artifact objects.

**Architecture:** Store the upstream npm-compatible metadata body as canonical cache content; parse and rewrite it with the current `root_url` whenever it is served. Keep artifact objects and descriptors unchanged in layout. Add a bounded revalidation window and remove metadata plus its descriptors when upstream returns 404.

**Tech Stack:** Rust, Tokio, serde/serde_json, axum, existing `NpmStorage`/`NpmService`.

**Spec:** `docs/reviews/2026-10-21-系统级工程评审.md` and the 2026-10-21 architecture discussion in the task thread.

## Global Constraints

- Do not change the artifact directory layout.
- Do not add user-facing configuration.
- Keep direct source terminal behavior; do not add sibling fallback to a direct proxy.
- Keep npm and UPM on the same implementation through `RegistryFlavor`.
- Do not clear npm/UPM canonical metadata only because `root_url` changed.
- Keep existing tests passing; final verification is 265 tests.

---

### Task 1: Canonical npm metadata storage

**Files:**
- Modify: `src/npm/storage.rs`
- Test: `tests/npm_cache_test.rs`

**Interfaces:**
- `StoredMetadata` gains `validated_at_unix_secs: u64`, `content_digest: String` and `source_fingerprint: String`.
- `NpmStorage::publish_metadata` gains a `source_fingerprint` parameter and stores the canonical upstream body.
- Add `NpmStorage::touch_metadata(&self, entry, package, key, digest, source_fingerprint) -> Result<(), StorageError>`.
- Add `NpmStorage::remove_metadata(&self, entry, package) -> Result<(), StorageError>`.

- [x] Add a failing storage test that publishes a canonical body, reads it back, and asserts the stored body is unchanged.
- [x] Add a failing storage test that removes metadata and verifies the pointer and body are gone.
- [x] Implement pointer fields with serde defaults and a canonical marker.
- [x] Reject non-canonical or digest-mismatched pointers as cache misses; delete the invalid entry.
- [x] Delete stale descriptor files not present in the new metadata publication.
- [x] Run `cargo test --test npm_cache_test`.

### Task 2: Render canonical metadata on read

**Files:**
- Modify: `src/npm/service.rs`
- Test: `tests/npm_service_test.rs`

**Interfaces:**
- `NpmService::metadata` returns a body rendered with the current `self.root_url`.
- A cache hit parses `StoredMetadata.body`, rewrites tarball URLs, and does not call upstream.
- A cache miss stores `response.body` (canonical), not the rewritten body.

- [x] Add a failing test that serves metadata through one router, then serves the same data directory through a second router with a different `root_url`; assert the second body contains the new URL and the upstream metadata request count stays at one.
- [x] Implement render-on-read and canonical publish-on-miss.
- [x] On parse or source-match failure, remove the cached metadata and treat it as a miss.
- [x] Run `cargo test --test npm_service_test`.

### Task 3: Revalidate stale metadata and handle deletion

**Files:**
- Modify: `src/npm/service.rs`, `src/npm/storage.rs`
- Test: `tests/npm_service_test.rs`

**Interfaces:**
- Add a fixed `NPM_METADATA_REVALIDATE_AFTER_SECS` (15 minutes).
- Stale cache re-fetches metadata from the bound source first, then the remaining group members in declaration order.
- 404 removes metadata and descriptors; network failure may serve stale with `metadata_stale` audit.

- [x] Add a failing test that ages the pointer, changes upstream metadata, and asserts the new metadata is served.
- [x] Add a failing test that ages the pointer, makes upstream return 404, and asserts the cached metadata is removed.
- [x] Implement the stale path without changing direct-source terminal behavior.
- [x] Run `cargo test --test npm_service_test`.

### Task 4: Keep npm/UPM metadata across `root_url` changes

**Files:**
- Modify: `src/main.rs`
- Test: `src/main.rs` unit tests

- [x] Change the root-url invalidation list so npm/UPM metadata is retained; Cargo, Docker and VS Code metadata are also retained because they are address-independent.
- [x] Update the existing invalidation test to assert npm metadata remains and a non-canonical ecosystem cache is removed.
- [x] Run `cargo test --bin stacktory`.

### Task 5: Documentation and verification

**Files:**
- Modify: `docs/architecture/npm-upm-mvp.md`
- Modify: `README.md` if the cache-invalidation statement is no longer exact.

- [x] Document canonical metadata, render-on-read, the 15-minute revalidation window, and the fact that root-url changes no longer clear npm/UPM metadata.
- [x] Run `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-targets --all-features`.
