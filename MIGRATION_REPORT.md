# Switchboard CLI — New API Migration Report

## Summary

Migrated the Switchboard CLI from the legacy GraphQL API to the new unified document-based API running on `localhost:4001/graphql`. All commands now target the new schema. Two command groups (`access` and `groups`) were removed as the new API has no permission management endpoints.

## Test Results

**All 42 tests pass:**
- 15 unit tests (shell_split, SVG rendering, Mermaid rendering)
- 27 integration tests (full end-to-end against local API)

```
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## API Changes

### Old API → New API mapping

| Old Pattern | New Pattern |
|---|---|
| `{ driveDocuments { ... } }` | `{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { ... } } }` |
| `{ driveDocument(idOrSlug: "x") { ... } }` | `{ document(identifier: "x") { document { ... } childIds } }` |
| `{ drives }` | `{ findDocuments(search: { type: "powerhouse/document-drive" }, paging: { limit: 1 }) { totalCount } }` |
| `driveIdBySlug(slug)` | `document(identifier)` (accepts both UUID and slug) |
| `addDrive(name, slug, ...)` | `DocumentDrive_createDocument(name)` + `DocumentDrive_setDriveName(...)` |
| `deleteDrive(id)` | `deleteDocument(identifier, propagate: CASCADE)` |
| `{Model} { getDocument(docId, driveId) }` | `document(identifier) { document { ... } }` |
| `{Model} { getDocuments(driveId) }` | `documentChildren(parentIdentifier) { items { ... } }` |
| `stateJSON` (string) | `state` (JSONObject) |
| `pushUpdates(...)` | `mutateDocument(documentIdentifier, actions)` |
| `job(id)` | `jobStatus(jobId)` |
| `{Model}_createDocument(name, driveId)` | `{Model}_createDocument(name, parentIdentifier)` |

### Key structural differences

- **Drives are documents** — type `powerhouse/document-drive`, queried via `findDocuments`
- **No separate drive queries** — `driveDocuments`, `driveDocument`, `driveIdBySlug` don't exist
- **`document(identifier)` returns nested structure** — `{ document: { document: {...}, childIds } }`
- **State is JSONObject** — not a string that needs parsing; drive nodes live at `state.global.nodes`
- **Search requires criteria** — `findDocuments(search: {})` fails; must provide `type`, `parentId`, or `identifiers`
- **No `/graphql/auth` subgraph** — permission management completely removed

## Files Changed

### Core migrations (GraphQL queries rewritten)

| File | Changes |
|---|---|
| `src/cli/drives.rs` | Full rewrite — list/get/create/delete all use new API |
| `src/cli/docs.rs` | Full rewrite — `fetch_drive_nodes()` helper fetches `state.global.nodes` per drive; `list` iterates drives and collects file nodes (falls back to `documentChildren`); `tree` renders folder/file hierarchy from nodes; `create`/`delete` use new mutations |
| `src/cli/ops.rs` | Uses `documentOperations(filter: { documentId })` |
| `src/cli/mutate.rs` | Uses `document(identifier)` for type discovery |
| `src/cli/helpers.rs` | Removed `resolve_drive_id()`, rewrote `resolve_doc()` and `select_drive()` |
| `src/cli/mod.rs` | Removed access/groups commands, updated `ping()` and `info()` queries |
| `src/cli/init.rs` | Updated connection test query |
| `src/cli/import_export.rs` | Uses `parentIdentifier` instead of `driveId` in create mutations |
| `src/cli/visualize.rs` | Uses `findDocuments` for drives |
| `src/cli/interactive.rs` | Removed access/groups completion, uses new queries |
| `src/cli/field_editor.rs` | Uses `document(identifier)` for state fetch |
| `src/cli/jobs.rs` | Uses `jobStatus(jobId)` |
| `src/graphql/introspection.rs` | Removed query type parsing (no namespace queries in new API) |

### Removed commands

| Command | Reason |
|---|---|
| `access show/grant/revoke/...` | No permission management API |
| `groups list/get/create/delete/...` | No group management API |

Files `src/cli/access.rs` and `src/cli/groups.rs` are empty (modules removed from `mod.rs`).

### Documentation updated

| File | Changes |
|---|---|
| `src/cli/guide.rs` | Removed access/groups references, updated GraphQL patterns, removed `/graphql/auth` endpoint |
| `tests/cli_integration.rs` | Rewrote tests for new API, added tests for removed commands, raw query, introspect, guide, auth |

## Commands Verified End-to-End

| Command | Status |
|---|---|
| `switchboard ping` | ✅ |
| `switchboard info` | ✅ |
| `switchboard drives list` | ✅ (table + json) |
| `switchboard drives get <id>` | ✅ |
| `switchboard drives create --name <n>` | ✅ |
| `switchboard drives delete <id> -y` | ✅ (single + multi) |
| `switchboard docs list --drive <id>` | ✅ |
| `switchboard docs get <id>` | ✅ |
| `switchboard docs tree --drive <id>` | ✅ |
| `switchboard ops <doc-id>` | ✅ |
| `switchboard models list` | ✅ |
| `switchboard introspect` | ✅ |
| `switchboard config list` | ✅ |
| `switchboard auth status` | ✅ |
| `switchboard query '<gql>'` | ✅ |
| `switchboard guide overview` | ✅ |
| Interactive REPL | ✅ |

## Implementation Notes

### `docs list` logic (matches old CLI)

1. If `--drive` specified: fetch that single drive's `state.global.nodes`, collect file nodes
2. If no `--drive`: fetch all drives via `findDocuments`, iterate each, collect file nodes across all drives (adds "Drive" column)
3. Fallback: when `state.global.nodes` is empty for a drive, falls back to `documentChildren(parentIdentifier)` for a flat document list
4. Supports visual formats (SVG/PNG/Mermaid) via shared `DriveTree` model

### `docs tree` logic (matches old CLI)

1. Fetches drive's `state.global.nodes`
2. Renders folder/file hierarchy using `parentFolder`-based tree traversal (same recursive approach as old CLI)
3. Fallback: when nodes empty, uses `documentChildren` for flat list display

### Key helper: `fetch_drive_nodes()`

Shared by `list`, `tree`, and `get` — queries `document(identifier) { document { id name state } }` and extracts `(id, name, nodes)` from `state.global.nodes`.

## Build

```
cargo clippy -- -D warnings   ✅ (0 warnings)
cargo build --release          ✅
cargo test                     ✅ (42/42)
```

Binary installed at `~/.cargo/bin/switchboard` (codesigned for macOS).
