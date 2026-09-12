# VS Code Marketplace Adapter Design

## Goal

Replace the current Open VSX-compatible VS Code adapter with a Microsoft Marketplace Gallery adapter so the VS Code extension side panel can search, view, install, and update extensions through Stacktory.

## Scope

The adapter serves the Gallery protocol required by VS Code's `extensions.gallery.serviceUrl`:

```text
POST /mirror/vscode/{entry}/extensionquery
GET  /mirror/vscode/{entry}/asset/{asset-id}
```

`{entry}` uses the existing `proxy` or `group` source graph. The first implementation supports the public Microsoft Marketplace Gallery API and its asset CDN. It does not implement extension publish, delete, account access, private Marketplace, ratings write operations, or the previous Open VSX `/api/...` protocol.

## Request Model

VS Code sends the Gallery query body to `extensionquery`. Stacktory accepts only JSON request bodies up to 1 MiB, forwards them as POST to:

```text
{proxy.base_url}/extensionquery
```

The proxy base URL for Microsoft Marketplace is:

```text
https://marketplace.visualstudio.com/_apis/public/gallery
```

Stacktory supplies fixed Gallery request headers:

```text
Accept: application/json;api-version=3.0-preview.1
Content-Type: application/json
X-Market-Client-Id: VSCode
```

The incoming JSON body is never interpreted as an upstream URL. Group selection applies only to the configured base URL and retains the existing retry, 404 fallback, and terminal client-error rules.

## Query Response And Asset Binding

Stacktory parses the Gallery query response and requires:

- `results` is an array.
- every returned extension has `publisher.publisherName`, `extensionName`, and `versions`.
- every version file has a nonempty `assetType` and an HTTP(S) `source` URL accepted by the configured-source URL policy.

Each upstream file URL is assigned:

```text
asset-id = SHA-256(source URL)
```

The response's `versions[].files[].source` value is rewritten to:

```text
{root_url}/mirror/vscode/{entry}/asset/{asset-id}
```

Stacktory writes the rewritten query response under:

```text
data_dir/vscode/metadata/{entry}/marketplace/{query-id}.json
```

`query-id` is SHA-256 of the exact request body. For every rewritten asset, Stacktory writes a descriptor under:

```text
data_dir/vscode/metadata/{entry}/marketplace/assets/{asset-id}.toml
```

The descriptor records the selected proxy, original asset URL, and expected Gallery asset type. The asset route can only resolve a descriptor already published by a successful query; it cannot request a client-supplied external URL.

## Artifact Storage

Marketplace assets are source-owned binary objects:

```text
data_dir/vscode/artifacts/{proxy}/objects/marketplace/{asset-id}
data_dir/vscode/artifacts/{proxy}/objects/marketplace/{asset-id}.toml
```

The sidecar preserves the upstream content type. `group` and direct proxy entries reuse an object only when they bind the same actual proxy and upstream asset URL. Marketplace responses do not provide a general SHA-256 contract for every asset, so Stacktory preserves official asset bytes and relies on the selected configured source; it does not claim additional content verification.

## Shared HTTP Boundary

The existing source client only supports GET bodies. It becomes a bounded request API:

- GET metadata and artifacts retain their current timeout classes.
- POST JSON supports the Gallery query body and fixed request headers.
- successful responses expose bytes and optional content type.

No adapter can choose an arbitrary method, upstream URL, redirect policy, or timeout outside this shared source boundary.

## HTTP Registry

The adapter registry request type becomes:

```rust
pub struct AdapterRequest {
    pub method: http::Method,
    pub path: String,
    pub body: Vec<u8>,
}
```

The generic `/mirror/{ecosystem}/{name}/{*path}` route accepts GET and POST and forwards only the method, bounded body, and path to registered adapters. Cargo rejects non-GET requests. The new VS Code adapter accepts only:

```text
POST extensionquery
GET  asset/{asset-id}
```

All other methods and paths return 404 or 405 without upstream access.

## VS Code Product Configuration

The root and deployment examples define:

```toml
[vscode]

[[vscode.sources]]
name = "official"
type = "proxy"
base_url = "https://marketplace.visualstudio.com/_apis/public/gallery"

[[vscode.sources]]
name = "group"
type = "group"
members = ["official"]
```

The VS Code integration target modifies an isolated copy of the VS Code product configuration:

```json
{
  "extensionsGallery": {
    "serviceUrl": "http://127.0.0.1:18761/mirror/vscode/group"
  }
}
```

The integration test uses a temporary copy of the installed VS Code runtime plus separate user-data and
extensions directories. It does not overwrite the user's normal VS Code product configuration, settings,
or extensions. User settings must not override `extensions.gallery.serviceUrl`, because current VS Code
uses that setting for a private Marketplace flow that requires local account entitlement.

## Error Boundaries

- Unknown entry, malformed asset ID, unsupported method, unsupported Gallery path, oversized query, or missing asset descriptor: `404` or `405`.
- Invalid query JSON, malformed Gallery response, or unsafe Gallery asset URL: `502`.
- Query source failure and asset source failure retain existing source error classification.
- Artifact cache and descriptor storage failure: `500`.

## Verification

Automated coverage must prove:

- the registry accepts POST only for the Marketplace adapter.
- query request body and fixed Gallery headers reach the configured upstream.
- Gallery result asset URLs are rewritten and descriptors bind the selected source.
- arbitrary asset IDs and external URLs cannot be fetched.
- Marketplace asset cache is reused by a group and its direct proxy.
- Cargo and Rustup continue to reject unsupported POST requests.

Real integration coverage must prove:

- Stacktory returns a real Microsoft Gallery query for `redhat.vscode-yaml`.
- VS Code 1.136.1 in an isolated user-data directory loads the configured Gallery service in the extensions side panel.
- selecting and installing an extension through the side panel makes at least one Stacktory `extensionquery` request and one `asset` request.
