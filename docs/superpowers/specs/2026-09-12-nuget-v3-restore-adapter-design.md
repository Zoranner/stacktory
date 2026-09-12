# NuGet V3 Restore Adapter Design

## Goal

Add a read-only NuGet V3 restore adapter that serves service index,
registration metadata, and package content through
`/mirror/nuget/{name}/...`, while preserving selected-proxy affinity and
preventing untrusted resource URLs from becoming download targets.

## Scope

The adapter supports:

```text
GET /mirror/nuget/{name}/v3/index.json
GET /mirror/nuget/{name}/registration/{*path}
GET /mirror/nuget/{name}/package/{id}/{version}/{id}.{version}.nupkg
```

It does not support search, push, delete, unlist, API keys, accounts,
permissions, symbol servers, or write APIs.

## Source Contract

Every NuGet proxy has one `base_url`, and that URL must be an actual V3 service
index JSON endpoint whose path ends in `/v3/index.json`. A group has no URL and
only references proxy members in order.

The adapter fetches the service index from the configured proxy URL. It accepts
only service index resources needed for restore:

- `RegistrationsBaseUrl` and versioned aliases.
- `PackageBaseAddress` and versioned aliases.
- search resources are validated as discovered metadata but are omitted from the
  client-visible index.

Unknown resources, publish resources, and unsupported capabilities are never
rewritten into usable local URLs.

## State And Relationships

Service index, registration documents, and package content are separate cached
objects. Each metadata object has a current representation and a sidecar with
the selected proxy and the validated upstream resource relationship.

The service index sidecar records the selected proxy and the discovered
registration/package resource URLs. Registration index/page/leaf sidecars record
the selected proxy, package ID/version, and the validated upstream relationship
for each rewritten package content URL.

A package request is accepted only when a previously published registration leaf
for the exact case-insensitive package ID and exact normalized version proves the
package relationship. The client cannot supply an upstream URL.

Group requests never merge registrations, package version sets, or service
indexes from multiple proxies. A later proxy is considered only after the
current source fails with a retryable failure, an allowed group 404, an invalid
resource relationship, or a package integrity failure. A successful object is
published atomically together with its selected-proxy relationship.

## URL And Path Safety

Configured service index URLs are parsed and validated before use. They must be
HTTP(S), credential-free, query-free, fragment-free, and end in the literal
`/v3/index.json` path. The existing configured-source URL policy remains the
authority for discovered resource URLs.

Client package IDs, versions, and registration paths reject empty segments,
backslashes, percent escapes, query/fragment markers, control characters,
`.`/`..` segments, and encoded path separators. Registration URLs are accepted
only when they are discovered from the selected service index and remain within
the validated registration resource scope. Package content URLs are accepted
only when they are discovered from the selected package registration leaf and
remain within the validated package content resource scope.

## Cache And Integrity

The cache layout is entry-owned for metadata and source-owned for package
content, with logical separation for service index, registration, and package
content. Cache keys include ecosystem, entry name, object role, package ID,
version, representation, and selected proxy.

Package downloads are written to a temporary file, checked against an upstream
registration digest when one is available, and atomically published only after
validation. If the upstream provides no usable digest, the adapter records a
local SHA-256 for cache consistency but does not claim upstream integrity
verification. Invalid content is never returned as a successful response and is
never published.

## Request Flow

1. Validate the entry and request path.
2. Load a valid service-index snapshot or select a proxy/group member and fetch
   the configured `/v3/index.json`.
3. Parse and validate resource relationships, rewrite only supported local URLs,
   and atomically publish the service-index snapshot and sidecar.
4. For registration requests, require the service-index binding, derive the
   discovered registration URL, validate the returned index/page/leaf
   relationship, rewrite only package content URLs, and publish metadata and
   relationship sidecars.
5. For package requests, require a matching registration leaf binding, fetch
   through the bound selected proxy and validated package resource, verify the
   content, atomically publish the source-owned package cache, and return bytes.
6. Record cache hits, publications, source selection, fallback, relationship
   failures, and integrity failures through the existing audit sink.

## Error Boundaries

- Unknown entry, invalid path, unknown package/version, missing relationship, or
  unbound content: `404`.
- Explicit upstream client errors such as `401` or `403`: preserve the status.
- Upstream unavailability, invalid JSON, unsafe resource relationships, or
  integrity failure after all permitted fallback attempts: `502`.
- Local cache, sidecar, atomic write, or audit failures: `500`.

## Verification

Focused local HTTP tests cover:

- service-index discovery and local URL rewriting;
- strict `/v3/index.json` configuration;
- registration index/page/leaf relationship validation;
- rejection of unknown resources, search routes, external client URLs, path
  traversal, and write methods;
- selected-proxy affinity and group fallback without source merging;
- package digest validation, atomic publication, cache isolation, and rejection
  of bad content.

After the local protocol suite passes, a separate real `dotnet restore` run
verifies client interoperability and is reported independently from local
tests.
