# Switchboard CLI — Specification

A standalone Rust CLI for interacting with remote Switchboard instances over GraphQL.
Fast, single-binary, zero runtime dependencies.

```
$ switchboard init
> Paste your Switchboard GraphQL URL: https://switchboard-staging.powerhouse.xyz/graphql
> Profile name [staging]: staging
✓ Connected. Introspecting schema...
✓ 19 document models discovered (Invoice, BuilderProfile, ResourceTemplate, ...)
✓ 27 drives found
✓ Profile "staging" saved as default
```

---

## Why Rust

This CLI is a **standalone tool** — it doesn't share code with the TypeScript monorepo.
It's a thin GraphQL client where the server does all validation and heavy lifting.
Rust gives us:

- **~5ms startup** — feels instant in the terminal
- **Single static binary** — `brew install`, `cargo install`, or download from GitHub Releases
- **No runtime** — no Node.js, no Python, nothing. Just the binary
- **Excellent ecosystem** — `clap` (CLI), `reqwest` (HTTP), `serde` (JSON), `tokio` (async)

---

## Critical Design Principle: Introspection-First

**Every Switchboard instance is different.** Staging has different document models
than production. A local dev server has different models than either. The CLI must
**never hardcode** document types, mutation names, or schema assumptions.

Instead, the CLI discovers everything dynamically via GraphQL introspection:

```
On `switchboard init` or `switchboard introspect`:
1. Run introspection query: { __schema { mutationType { fields { name } } } }
2. Extract all *_createDocument mutations → derive available document types
   e.g. "Invoice_createDocument" → type "powerhouse/invoice", prefix "Invoice"
3. Extract all *_<operation> mutations → derive available operations per model
   e.g. "Invoice_editInvoice", "Invoice_setStatus" → operations for Invoice
4. Cache the schema locally in ~/.switchboard/cache/<profile>.json
5. Re-introspect on demand (`switchboard introspect`) or automatically when a model is missing
```

The cache includes all document models, including `DocumentDrive` (type `powerhouse/document-drive`), enabling drive mutations via `docs mutate`. Commands like `docs mutate` and `docs create` will automatically re-introspect if the required model is missing from the cache (e.g., after a reactor restart that loads new packages).

This cache powers:
- Tab completion for document types and operations
- Validation of `--type` arguments before sending requests
- The `models list` command (no `documentModels` query exists on the API)
- Interactive prompts showing available options

---

## Verified API Patterns

Tested against staging (`switchboard-staging.powerhouse.xyz`) and local (`localhost:4001`):

### Endpoints

The primary gateway federates everything:

```
{base}/graphql          # Apollo Gateway — all queries and mutations
```

Two additional subgraphs are available for direct access when needed:

```
{base}/graphql/r        # Reactor subgraph — document ops + WebSocket subscriptions
{base}/graphql/auth     # Auth subgraph — permissions, groups (only when auth enabled)
```

**The CLI uses `/graphql` for all standard operations.** The subgraphs are useful for:

- **`/graphql/r`** — WebSocket subscriptions (`watch` command), since it exposes `hasSubscriptions = true`
- **`/graphql/auth`** — Direct permission queries when auth is configured

### Read Queries (verified working)

```graphql
# List drive IDs (returns string[])
{ drives }

# Resolve slug to ID
{ driveIdBySlug(slug: "builders") }

# List all drives with metadata
{ driveDocuments { id name slug documentType revision } }

# Get single drive with full node tree
{
  driveDocument(idOrSlug: "builders") {
    id name slug revision
    state {
      name icon
      nodes {
        ... on DocumentDrive_FileNode { id name kind documentType parentFolder }
        ... on DocumentDrive_FolderNode { id name kind parentFolder }
      }
    }
  }
}

# List documents of a specific type in a drive (model-specific namespace)
{ BuilderProfile { getDocuments(driveId: "liberuum-drive") { id name documentType revision stateJSON } } }
{ Invoice { getDocuments(driveId: "my-drive-uuid") { id name documentType revision stateJSON } } }

# Get a specific document by ID (model-specific namespace)
{ Invoice { getDocument(docId: "uuid-here", driveId: "drive-uuid") { id name stateJSON } } }
```

### Mutations (verified working on localhost:4001)

```graphql
# Create drive (interactive — user provides name, icon, preferredEditor)
mutation {
  addDrive(name: "my-drive", icon: "https://...", preferredEditor: "builder-team-admin") {
    id slug name icon preferredEditor
  }
}

# Delete drive (MUST use UUID, not slug)
mutation { deleteDrive(id: "uuid-here") }

# Create document (model-specific — discovered via introspection)
mutation { Invoice_createDocument(name: "Q1 Invoice", driveId: "uuid-here") }

# Mutate a document (model-specific operations)
mutation { Invoice_editInvoice(docId: "uuid-here", input: { amount: 2000 }) }
```

### Key Quirks Discovered

- **`documentModels` query does NOT exist** — must use introspection instead
- **`deleteDrive(id:)` silently succeeds with slugs** but doesn't actually delete — must use UUID
- **`_createDocument(driveId:)` requires UUID** not slug — CLI must resolve slug→UUID first
- **Document state** is always accessed via `stateJSON` field (raw JSON), not typed fragments
- **Model-specific queries** use namespace pattern: `{ ModelName { getDocument(...) } }`
- **Model-specific mutations** use prefix pattern: `ModelName_operationName(docId, input)`
- **Nested API variant** — some instances use nested mutations: `{ DocumentDrive { createDocument(...) } }` instead of flat `DocumentDrive_createDocument(...)`. The CLI auto-detects and supports both.

---

## Feature Specification

### 1. Setup & Configuration

```
switchboard init
switchboard config list
switchboard config use <profile>
switchboard config remove <profile>
switchboard config show
```

| Feature | Details |
|---------|---------|
| First-run wizard | Prompt for GraphQL URL, validate connection, store as default profile |
| Schema introspection | Discover document types, cache schema locally |
| Named profiles | `~/.switchboard/profiles.toml` — store multiple instances |
| Active profile | One profile active at a time, switchable via `config use` |
| Auth token storage | Per-profile bearer token (JWT) for authenticated endpoints |
| Connection test | `init` validates the URL by running `{ drives }` |

Profile file structure:

```toml
[profiles.staging]
url = "https://switchboard-staging.powerhouse.xyz/graphql"
default = true

[profiles.dev]
url = "https://switchboard-dev.powerhouse.xyz/graphql"
token = "eyJhbGciOiJFUzI1NiIs..."

[profiles.local]
url = "http://localhost:4001/graphql"
```

---

### 2. Introspection & Schema Discovery

This is the backbone of the CLI. Each instance has different document models.

```
switchboard introspect                    # Re-discover schema from current instance
switchboard models list                   # List discovered document types
switchboard models get <type>             # Show operations available for a type
switchboard schema                        # Dump full GraphQL schema
switchboard ping                          # Quick connection health check
switchboard info                          # Drive count, model count, server info
```

How `models list` works under the hood:

```
1. Read cached introspection from ~/.switchboard/cache/<profile>.json
2. If no cache, run introspection query against {base}/graphql
3. Parse __schema.mutationType.fields
4. Filter for *_createDocument → extract model prefixes
5. Convert PascalCase to kebab → "Invoice" → "powerhouse/invoice"
6. Display as table
```

Example output:

```
$ switchboard models list
┌───────────────────────────────────┬─────────────────────┐
│ Type                              │ Mutation Prefix      │
├───────────────────────────────────┼─────────────────────┤
│ powerhouse/invoice                │ Invoice              │
│ powerhouse/builder-profile        │ BuilderProfile       │
│ powerhouse/resource-template      │ ResourceTemplate     │
│ powerhouse/service-offering       │ ServiceOffering      │
│ powerhouse/expense-report         │ ExpenseReport        │
│ powerhouse/scope-of-work          │ ScopeOfWork          │
│ ...                               │ ...                  │
└───────────────────────────────────┴─────────────────────┘

$ switchboard models get powerhouse/invoice
Type: powerhouse/invoice
Prefix: Invoice
Available mutations:
  Invoice_createDocument(name!, driveId)
  Invoice_editInvoice(docId!, input!)
  Invoice_setStatus(docId!, input!)
  Invoice_addLineItem(docId!, input!)
  ...
```

---

### 3. Drives

```
switchboard drives list
switchboard drives get <id-or-slug>                          # Also supports --format svg/png/mermaid --out <file>
switchboard drives create [--name <name>] [--icon <url>] [--preferred-editor <editor>]
switchboard drives delete <ids...> [-y]
switchboard drives check <id-or-slug>                        # Scan for ghost nodes
switchboard drives fix <id-or-slug> [-y]                     # Remove ghost nodes
```

**`drives create` is interactive** — if `--name` is omitted, the user is prompted.
Additional fields like `--icon` and `--preferred-editor` are optional:

```
$ switchboard drives create
> Drive name: liberum-drive
> Icon URL (optional): https://cdn-icons-png.flaticon.com/512/1144/1144760.png
> Preferred editor (optional): builder-team-admin

✓ Drive created
  ID:    47cda535-6b7a-4c0e-8260-acb903f4c4fa
  Slug:  liberum-drive
  Name:  liberum-drive
```

All fields can also be passed as flags for scripting:

```
switchboard drives create \
  --name "liberum-drive" \
  --icon "https://..." \
  --preferred-editor "builder-team-admin"
```

**`drives delete` resolves slugs automatically and supports multi-delete:**

```
$ switchboard drives delete liberuum-drive another-drive -y
  Resolved slug "liberuum-drive" → UUID 47cda535-...
  Resolved slug "another-drive" → UUID e5f6g7h8-...
✓ 2 drives deleted
```

Maps to GraphQL:

- `{ driveDocuments { id name slug } }` → list drives with metadata
- `{ driveDocument(idOrSlug: ...) { ... state { nodes { ... } } } }` → get drive tree
- `{ driveIdBySlug(slug: ...) }` → resolve slug → UUID
- `mutation { addDrive(name, icon, preferredEditor) }` → create
- `mutation { deleteDrive(id: UUID) }` → delete (resolves slug→UUID first)

---

### 4. Documents

```
switchboard docs list [--drive <slug>] [--type <type>]       # Also supports --format svg/png/mermaid --out <file>
switchboard docs get <id-or-name> [--drive <slug>] [--state] [--out <file>]
switchboard docs tree [<slug>]
switchboard docs create [--type <type>] [--name <name>] [--drive <slug>]
switchboard docs delete <ids-or-names...> [-y]
switchboard docs rename <id-or-name> <new-name>
switchboard docs parents <id-or-name>
switchboard docs add-to <parent> <ids...>
switchboard docs remove-from <parent> <ids...>
switchboard docs move <ids...> --from <src> --to <dst>
```

**Name resolution**: Most document commands accept UUIDs, names, or slugs. The CLI resolves names by searching across drives. Use `--drive` to narrow ambiguous name lookups.

**`docs list` shows the drive's file tree:**

```
$ switchboard docs list --drive liberuum-drive
┌──────────────────────────────────────┬──────────────────┬──────────────────────────────────┐
│ ID                                   │ Name             │ Type                             │
├──────────────────────────────────────┼──────────────────┼──────────────────────────────────┤
│ 3ac3588f-...                         │ liberuum         │ powerhouse/builder-profile        │
│ 1fea2d87-...                         │ new service      │ powerhouse/resource-template      │
│ 136130ed-...                         │ offering         │ powerhouse/service-offering       │
└──────────────────────────────────────┴──────────────────┴──────────────────────────────────┘
```

**`docs tree` shows the hierarchical view:**

```
$ switchboard docs tree liberuum-drive
liberum-drive/
├── liberuum (powerhouse/builder-profile)
├── 📁 Expense Reports/
├── 📁 Service Subscriptions/
├── 📁 Services And Offerings/
│   ├── new service (powerhouse/resource-template)
│   └── offering (powerhouse/service-offering)
```

**`docs get` returns document details. Use `--state` for full state:**

```
$ switchboard docs get liberuum --state --format json
{
  "id": "3ac3588f-...",
  "name": "liberuum",
  "documentType": "powerhouse/builder-profile",
  "revision": 14,
  "state": { ... }
}
```

**`docs create` is interactive — uses introspected model list:**

```
$ switchboard docs create
> Select document type:
  1) powerhouse/invoice
  2) powerhouse/builder-profile
  3) powerhouse/resource-template
  ...
> Choice: 1
> Document name: Q1 Invoice
> Drive (slug or ID): liberuum-drive
  Resolved slug → UUID 47cda535-...

✓ Document created
  ID: 41d2cae7-f9b0-4038-8a52-1e2b5cf6cc2b
```

Or with flags:

```
switchboard docs create --type powerhouse/invoice --name "Q1 Invoice" --drive liberuum-drive
```

Maps to GraphQL (all on `/graphql`):

- `{ Model { getDocuments(driveId:) { id name stateJSON } } }` → list docs by type
- `{ Model { getDocument(docId:, driveId:) { id name stateJSON } } }` → get single doc
- `{ driveDocument(idOrSlug:) { state { nodes { ... } } } }` → get folder tree
- `mutation { Model_createDocument(name:, driveId: UUID) }` → create (model-specific)
- `mutation { deleteDocument(id:) }` → delete
- `renameDocument(documentIdentifier, name)` → rename
- `documentIncomingRelationships(targetIdentifier, relationshipType: "child")` → reverse tree traversal
- `documentOutgoingRelationships(sourceIdentifier, relationshipType: "child")` → forward tree traversal
- `DocumentDrive { addFile(docId, input: { id, name, documentType }) }` → add doc to drive
- `DocumentDrive { deleteNode(docId, input: { id }) }` → remove doc from drive
- Remove from source + add to target → move between drives

---

### 4a. Folders

```
switchboard folders create --name <name> [--parent <id-or-name>] [--drive <id-or-slug>]
switchboard folders delete <id> --drive <id-or-slug> [-y]
```

Folders are nodes inside a drive's `state.global.nodes` (kind = `"folder"`); they
are not separate documents. The CLI hides this behind a typed command.

**`--parent` is universal:** it accepts a drive (folder is created at the drive
root) or a folder (folder is nested inside it). Resolution order:

1. UUID match → used directly. Drive vs folder is determined by querying the
   document type.
2. Drive name/slug → folder placed at that drive's root.
3. Folder name → search all drives for a folder with that name. If a single
   match is found, use that drive and folder. If multiple drives contain a
   folder with the same name, the user must disambiguate with `--drive`.

**`--folder` is a backwards-compat alias for `--parent`.** Tab completion
treats `--folder` strictly: only folder candidates are surfaced, while
`--parent` lists both drives and folders with `(drive)` / `(folder in <slug>)`
labels.

Either `--parent` or `--drive` (or both) must be passed. Examples:

```
folders create --name "Reports" --drive my-builder-team-admin
folders create --name "2026" --parent Reports
folders create --name "Q1" --drive my-builder-team-admin --parent Reports
```

Maps to GraphQL:

- `mutation { DocumentDrive { addFolder(docId: <drive-id>, input: { id, name, parentFolder? }) } }`
- `mutation { DocumentDrive { deleteNode(docId: <drive-id>, input: { id }) } }`

The folder UUID is generated client-side. Children of a deleted folder are
**not** auto-removed; callers must move or delete them first.

---

### 5. Document Mutations

```
switchboard docs mutate <doc-id-or-name> [--op <OPERATION>] [--input '<json>'] [--input-file <FILE>] [--drive <slug>]
```

Uses the model-specific mutation discovered via introspection. The `--drive` flag is
optional — the CLI auto-detects the drive by searching all drives for the document.

**Field-by-field editor** (default when `--op` and `--input` are omitted):

```
$ switchboard docs mutate liberuum
> Select operation:
  1) updateProfile
  2) addSkill
  3) setStatus
> Choice: 1

  name [Powerhouse]: New Name
  description [A team...]:            ← Enter to keep current
  social.twitter [@alice]: @bob
  tags ["rust", "web3"]
    > Keep current / Add / Remove / Replace / Clear

  Input (changed fields only):
  { "name": "New Name", "social": { "twitter": "@bob" } }
  Apply mutation? [Y/n]
✓ Mutation applied.
```

**Raw JSON mode** (for scripting):

```
$ switchboard docs mutate 41d2cae7-... --op editInvoice --input '{"amount": 2000}'
  Running: Invoice_editInvoice(docId: "41d2cae7-...")
✓ Mutation applied.
```

**File input mode** (for complex JSON with special characters):

The `--input-file` flag reads input JSON from a file (or stdin with `-`), bypassing
shell escaping issues with characters like `\n`, `!`, `{}`, etc. This is especially
useful for AI agents and scripts that need to pass multiline content (e.g. GraphQL SDL
schemas) as JSON values. The mutate command uses GraphQL variables instead of string
interpolation, which properly handles special characters in values.

```
# From a file
$ switchboard docs mutate <doc-id> --op setStateSchema --input-file schema.json

# From stdin
$ echo '{"scope": "global", "schema": "type Foo {\n  bar: String\n}"}' | switchboard docs mutate <doc-id> --op setStateSchema --input-file -
```

The field editor uses live `__type` introspection to discover input fields, supports
nested objects, enums (displayed as select pickers), arrays (add/remove/replace),
and booleans (confirm prompts). Current document state is pre-populated via `stateJSON`.

Maps to GraphQL (all on `/graphql`):

```graphql
# Model-specific mutations (discovered via introspection)
mutation { Invoice_editInvoice(docId: "uuid", input: { amount: 2000 }) }
mutation { BuilderProfile_updateProfile(docId: "uuid", input: { name: "New Name" }) }
```

---

### 6. Document Apply (Raw Actions)

```
switchboard docs apply <id-or-name> --actions '<json_array>'
switchboard docs apply <id-or-name> --file <path>            # Read actions from file (- for stdin)
switchboard docs apply <id-or-name> --file <path> --wait     # Wait for async completion
```

Dispatches raw actions to a document via `mutateDocumentAsync`. Returns a job ID.
With `--wait`, blocks until the job completes.

The CLI automatically injects `timestampUtcMs` (ISO-8601 format, e.g. `"2026-03-22T22:06:53.528Z"`) into each action if missing. This is required by the reactor's operation store (which does `new Date(timestampUtcMs)`) but not populated by the generic `mutateDocument` resolver — without it, drive operations (ADD_FOLDER, MOVE_NODE, etc.) fail with "Invalid time value".

```
$ switchboard docs apply abc123 --actions '[{"type":"SET_NAME","input":{"name":"New Name"}}]'
Job ID: job-456

$ switchboard docs apply abc123 --file actions.json --wait
Job ID: job-789
⠋ Waiting for job to complete...
✓ Job completed successfully.
```

Maps to GraphQL:

```graphql
mutation($documentIdentifier: String!, $actions: [JSONObject!]!) {
  mutateDocumentAsync(documentIdentifier: $documentIdentifier, actions: $actions)
}
```

---

### 7. Import & Export (.phd Files)

The `.phd` format is a ZIP archive containing document data. The CLI supports
importing and exporting documents in this format, compatible with the Powerhouse
ecosystem's drag-and-drop workflow.

#### Export

```
switchboard export all [-o ./dir/]                           # Export everything
switchboard export drive <slug> [-o ./dir/]                  # Export all docs in a drive
switchboard export doc <id> --drive <slug> [-o file.phd]     # Export a single document
```

**All export commands support operation filters:**

| Flag | Description |
|------|-------------|
| `--action-types <TYPES>` | Comma-separated action types to include |
| `--since-revision <N>` | Only operations from revision N onwards |
| `--from <ISO-8601>` | Only operations from this timestamp |
| `--to <ISO-8601>` | Only operations up to this timestamp |

Examples:

```
# Export everything
$ switchboard export all --out ./backup/
  Exporting 27 drives...
✓ All drives exported to ./backup/

# Export one drive
$ switchboard export drive liberuum-drive --out ./downloads/
  Fetching drive info...
  Name: liberum-drive (6 files, 3 folders)
  Downloading 6 documents...
  [1/6] liberuum (powerhouse/builder-profile) → 12.3 KB ✓
  [2/6] new service (powerhouse/resource-template) → 8.1 KB ✓
  ...
✓ 6 documents saved to ./downloads/liberum-drive/

# Export with filters
$ switchboard export drive builders --since-revision 100 -o ./incremental/
$ switchboard export all --from 2026-01-01T00:00:00Z --to 2026-03-01T00:00:00Z -o ./q1/
$ switchboard export all --action-types SET_NAME,ADD_ITEM -o ./filtered/
```

The .phd ZIP contains:

```
document.phd (ZIP)
├── header.json        # { id, documentType, name, revision, timestamps }
├── state.json         # Initial empty state
├── current-state.json # Current document state (stateJSON from API)
└── operations.json    # Full operation history { global: [...] }
```

#### Import

```
switchboard import <files...> --drive <slug> [--strict] [--id-mapping <file>]
```

**Flags:**

- `--strict` — treats per-op failures and state mismatches as hard errors. Without this flag,
  failures are reported per-document and the import continues; with it, the first failure
  ends the run with a non-zero exit code. Use this in CI or scripted batch imports where
  silent partial failures would be a data-integrity risk.
- `--id-mapping <file>` — JSON object mapping `{ "<old-uuid>": "<new-uuid>", ... }`. Applied
  to op inputs as they are dispatched, so cross-document references survive across reactor
  reassignment. Within a single invocation the CLI also builds this map automatically as
  documents are created (the new doc's old UUID → new UUID), so a multi-doc import within
  one process keeps internal references consistent without an explicit mapping file.

**Forward-reference deferral (default behavior):** Cross-document references
in op inputs are rewritten using an old → new UUID map built as documents are
created during import. Because docs are created sequentially, a knowledge-note
op like `ADD_LINK { targetDocumentId: <uuid> }` may run *before* the doc that
`<uuid>` refers to has been created — at which point the map is incomplete and
the rewrite would skip. Without intervention, that link ends up pointing at
the source's old UUID and silently breaks on the destination.

The CLI handles this with a deferral queue:

1. For each op, scan inputs recursively for UUID-shaped strings.
2. If every UUID is either in the id_map or not a UUID → rewrite + dispatch
   immediately (the common case).
3. If any UUID is unknown → enqueue the op as a `DeferredOp { doc_id, doc_type, op }`.
4. After every input has been processed (every doc created), drain the queue:
   re-rewrite each input with the now-complete map and dispatch.

Any UUID still missing at drain time is an external reference (pointing at a
doc outside this import) and is dispatched as-is, identical to the
pre-deferral behavior. Per-doc verdict shows the deferred count separately:
`Ops:    1 pushed, 19 deferred (forward refs) of 20`. The drain step prints
`Drained: N resolved, M failed`.

Verified on the 392-doc `bai/knowledge-vault` test drive (738 inter-note
links, 2,542 ops with forward refs): every link to a doc inside the import
resolved correctly. Pre-deferral, ~52% of bidirectional links survived;
post-deferral, **100% of in-import refs** resolve. Phantom refs (pointing at
docs that don't exist anywhere) pass through unchanged — those are data
quality issues in the source, not CLI bugs.

**Folder reconstruction:** When a `<files...>` argument is a directory, `import`
walks it recursively and uses the relative sub-paths to recreate the folder
hierarchy on the destination drive. Existing folders are reused (matched by
full path from drive root); missing ones are created via `DocumentDrive_addFolder`.
Each new doc is then placed in its target folder via `DocumentDrive_moveNode`.
Plain file arguments (or files inside an explicit directory) land at drive root.
Empty folders in the source are not preserved (the .phd export format is
file-based — empty folders never appear on disk).

**Verdict semantics:**

- `✓ EXACT MATCH` — every op applied and the resulting state matches the .phd's `current-state.json`.
- `✓ Imported (state has drift on volatile fields)` — every op applied; state diverges only
  on fields like `lastModified` that the reactor stamps when ops are replayed.
- `⚠ states equal but N op(s) failed — content may be missing` — some ops were rejected,
  but the resulting state happens to JSON-equal the expected (often because both are empty).
  This is the silent-corruption case the bug report calls out.
- `⚠ Imported with errors` — at least one op was rejected by the reactor.

```
$ switchboard import invoice.phd "expense report.phd" --drive liberuum-drive
  Discovering document types...
  Found 19 types

  ── invoice.phd ──
  Type: powerhouse/invoice
  Name: Q1 Invoice
  Ops:  42 global
  Creating Invoice document...
  Created: 41d2cae7-...
  Pushing 42 operations (1 batch)...
  [1/1] 42 ops → revision 42
  Verifying state... EXACT MATCH
✓ invoice.phd uploaded successfully

  ── expense report.phd ──
  ...
```

The import flow:

1. Read header.json from .phd → get documentType and name
2. Introspect API → find matching `_createDocument` mutation for this type
3. Create the document via `Model_createDocument(name, driveId)`
4. Read operations.json from .phd
5. Replay operations by calling model-specific mutations sequentially
6. Verify state by comparing `stateJSON` from API with `current-state.json` from .phd

---

### 7a. Drive Migration

```
switchboard migrate <source-drive> --from <profile> --to <profile>
```

Moves a drive between two Switchboard profiles in one invocation, producing a
byte-for-byte equivalent drive on the destination — same drive UUID, same
contained-document UUIDs, same operation history end-to-end.

**Behavior (fixed; no flags):**

- Always preserves the source drive's UUID on the destination.
- Always preserves every contained document's UUID.
- Always replays the full operation history (document scope + drive scope +
  any custom scopes).
- Always strict: any failed `mutateDocumentAsync` submission or async job
  failure aborts immediately.
- Source drive is left untouched (copy, not move).
- Refuses to run if the destination already has a drive with the same slug
  (no auto-suffix, no overwrite, no prompt).

**Mechanism.** Unlike `import`, which uses the typed model-specific
`createDocument` mutation (auto-generates IDs server-side), `migrate`
submits raw actions directly via `mutateDocumentAsync`. The `CREATE_DOCUMENT`
action carries the source UUID in its `input.id`, so the destination reactor
materialises the document with that exact ID. The same path replays
`SET_DRIVE_NAME`, `SET_DRIVE_ICON`, `ADD_FILE`, `ADD_FOLDER`, and every
document-level op verbatim, preserving timestamps and action IDs.

**Output:** documents migrated (drive + children) and total operations
replayed.

---

### 8. Operations History

```
switchboard ops <doc-id-or-name> [--drive <slug>] [--skip N] [--first N]
```

```
$ switchboard ops 3ac3588f-... --first 5
┌───────┬─────────────────┬──────────────────────────┬──────────────────────────┐
│ Index │ Type            │ Timestamp                │ Input                    │
├───────┼─────────────────┼──────────────────────────┼──────────────────────────┤
│ 0     │ SET_NAME        │ 2026-02-06T12:12:44.796Z │ name: liberuum           │
│ 1     │ ADD_SKILL       │ 2026-02-06T12:13:01.123Z │ skill: RUST              │
│ 2     │ SET_STATUS      │ 2026-02-06T12:13:15.456Z │ status: ACTIVE           │
│ 3     │ ADD_SCOPE       │ 2026-02-06T12:14:02.789Z │ scope: Protocol          │
│ 4     │ SET_DESCRIPTION │ 2026-02-06T12:14:30.012Z │ description: A team...   │
└───────┴─────────────────┴──────────────────────────┴──────────────────────────┘
Showing 5 of 14 operations
```

Supports drive documents (type `powerhouse/document-drive`) as well as file nodes.
When the document is a drive itself, falls back to the drive-scoped endpoint:

```bash
switchboard ops jazzman/               # Operations on the jazzman drive itself
switchboard ops jazzman/my-doc         # Operations on my-doc inside jazzman
switchboard ops my-doc                 # Auto-detect drive (existing behavior)
switchboard ops my-doc --drive jazzman # Explicit drive
```

Maps to GraphQL (via model-specific namespace, with drive-scoped fallback):

```graphql
{
  Invoice {
    getDocument(docId: "...") {
      operations { id type index timestampUtcMs hash skip inputText error }
    }
  }
}
```

---

### 9. Raw GraphQL

```
switchboard query '<graphql>'
switchboard query '<graphql>' --variables '<json>'
switchboard query --file ./my-query.graphql
switchboard query --file ./mutation.graphql --variables '{"name":"test"}'
```

Escape hatch for anything not covered by dedicated commands.
All queries go through the main `/graphql` endpoint.

---

### 10. Authentication

**Auth is optional.** If no token is configured, the CLI sends plain GraphQL
requests with no `Authorization` header — this works for open instances. If the
server returns 401/403, the CLI prompts the user to authenticate.

When a token *is* configured, every request automatically includes
`Authorization: Bearer <token>`.

```
switchboard auth login [--token <jwt>]
switchboard auth logout
switchboard auth status
switchboard auth token                    # Print current token
```

| Method | Details |
|--------|---------|
| Bearer token | Paste a JWT directly (`--token`) or interactively |
| Environment | `SWITCHBOARD_TOKEN` env var override (highest priority) |

**Priority order:** `SWITCHBOARD_TOKEN` env var > profile token > no auth.

Token stored per-profile in `~/.switchboard/profiles.toml`.

```toml
[profiles.staging]
url = "https://switchboard-staging.powerhouse.xyz/graphql"
# no token — open API, works without auth

[profiles.dev]
url = "https://switchboard-dev.powerhouse.xyz/graphql"
token = "eyJhbGciOiJFUzI1NiIs..."  # required for this instance
```

---

### 11. Analytics

```
switchboard analytics metrics                                # List available metrics
switchboard analytics dimensions                             # List dimensions and their values
switchboard analytics currencies                             # List available currencies
switchboard analytics series [--start <date>] [--end <date>] [--granularity <g>] [--metrics <m>] [--currency <c>]
```

**Granularity options**: `HOURLY`, `DAILY`, `WEEKLY`, `MONTHLY`, `ANNUALLY`, `TOTAL`

Maps to GraphQL:

- `analytics { metrics }` → list metrics
- `analytics { dimensions { name values { path label } } }` → list dimensions
- `analytics { currencies }` → list currencies
- `analytics { series(filter: { ... }) { period rows { metric value unit sum } } }` → time series

> **Note:** The `access` and `groups` commands from the legacy API have been removed.
> The new API has no permission management endpoints.

---

### 12. Real-Time Subscriptions (WebSocket)

```
switchboard watch docs [--type <type>] [--drive <slug>] [--doc <id>] [--exec <cmd>]
switchboard watch job <job-id>
```

Connects to the reactor subgraph via WebSocket (`wss://{host}/graphql/r`) and streams events:

- `documentChanges(search, view)` → CREATED, UPDATED, DELETED, CHILD_ADDED, etc.
- `jobChanges(jobId)` → job status updates for async mutations

Output streams as newline-delimited JSON for piping:

```bash
# Stream all changes as JSON
switchboard watch docs --format json | jq '.documentId'

# Filter by drive or document type
switchboard watch docs --drive my-drive --format json
switchboard watch docs --type powerhouse/invoice --format json

# Filter by specific document
switchboard watch docs --doc abc123 --format json

# Execute a command for each event (receives JSON on stdin, $SWITCHBOARD_EVENT set)
switchboard watch docs --exec './on-change.sh' --format json
switchboard watch docs --exec 'curl -sX POST -d @- https://hooks.example.com/notify' --format json
```

---

### 13. Async Job Tracking

```
switchboard jobs status <job-id>                             # Get current status
switchboard jobs wait <job-id> [--interval <secs>] [--timeout <secs>]  # Block until complete
switchboard jobs watch <job-id>                              # Stream updates via WebSocket
```

**`jobs wait` defaults:** interval = 2 seconds, timeout = 300 seconds (0 = no timeout).

For long-running mutations dispatched via `mutateDocumentAsync` (e.g., `docs apply`).

---

### 14. Sync Channels

```
switchboard sync touch <channel-input>                       # JSON or @path/to/file.json
switchboard sync push <envelopes>                            # JSON or @path/to/file.json
switchboard sync poll <channel-id> [--ack <N>] [--latest <N>]
```

Maps to GraphQL:

- `touchChannel(input)` → create/update sync channel
- `pushSyncEnvelopes(envelopes)` → push operations
- `pollSyncEnvelopes(channelId, outboxAck, outboxLatest)` → poll

---

### 15. Output Formatting (Global Flags)

Every command supports:

```
--format table       # Human-readable table (default for TTY)
--format json        # Machine-readable JSON (default for pipes)
--format raw         # Raw GraphQL response
--format svg         # Powerhouse-themed SVG diagram
--format png         # Rasterized PNG diagram
--format mermaid     # Mermaid flowchart markup
--quiet              # Suppress headers, just output data
--no-color           # Disable color output
-p, --profile <name> # Use a specific profile
```

Automatic detection: if stdout is a pipe, default to JSON. If TTY, default to table.

Visual formats (`svg`, `png`, `mermaid`) are supported on:
- `visualize` — all drives and documents
- `drives get` — single drive tree
- `docs list` — documents in a drive
- `docs get` — document state as a themed card

---

### 16. Visualization

```
switchboard visualize                              # Terminal tree (default)
switchboard visualize --format json                # Hierarchical JSON
switchboard visualize --format svg --out map.svg   # Powerhouse-themed SVG diagram
switchboard visualize --format png --out map.png   # Rasterized PNG diagram
switchboard visualize --format mermaid             # Mermaid flowchart markup
```

**Aggregation**: Fetches all drives, then all nodes per drive in parallel, then enriches
file-level nodes with revision metadata by querying each model namespace's `getDocuments`
in parallel. Builds a unified `DriveTree` data model consumed by all renderers.

**Formats**:

- `table` (default): Terminal tree output with folder/file hierarchy, unicode tree connectors
- `json`: Serialized `DriveTree` — hierarchical JSON with drives, folders, files, metadata
- `svg`: Powerhouse-themed vector diagram (dark background, cyan drives, purple folders, green docs, blue connecting lines). Built programmatically — no SVG crate dependency
- `png`: Rasterized SVG via `resvg` + `usvg` + `tiny-skia`. Requires `--out` when stdout is a TTY
- `mermaid`: `graph TD` flowchart with Powerhouse-themed style directives. Renders in GitHub, Notion, etc.

**Theme** (SVG/PNG):

- Background: `#0E0E0D`, Surface: `#14151A`, Border: `rgba(255,255,255,0.14)`
- Drive accent: `#04D9EB` (cyan), Folder accent: `#7A3AFF` (purple), Doc accent: `#07C262` (green)
- Connecting lines: `#0285FF` (blue), Font: Inter, system-ui, sans-serif

---

### 17. Interactive REPL Mode

```
switchboard interactive
switchboard -i
```

Launches an interactive session with:

- **Full CLI parity** — every command works inside the REPL
- Tab completion for commands, drive slugs, document names, profile names, model types, guide topics
- Hierarchical `drive/doc` completion for `ops` (e.g., `ops jazzman/[Tab]` lists docs inside jazzman)
- Drive-scoped doc completion after `--drive <slug>`
- Visual command separators for readability
- Animated loading spinners during API queries
- Automatic profile switching with client and completion refresh
- Guide topic shortcuts (e.g., `overview` instead of `guide overview`)
- Shell-like quoting (single, double, backslash escapes)
- Per-command flag overrides (`--format`, `--profile`, `--quiet`)
- `--help` passthrough on any command
- Raw GraphQL shorthand — type `query { ... }` directly without quotes
- Persistent history across sessions (`~/.switchboard/history`)
- Arrow keys for history, Ctrl+C to cancel, Ctrl+D to exit

```
staging> drives list

──── drives list ────────────────────────────────────────────
┌──────────────────┬──────────────┬──────────────┐
│ ID               │ Name         │ Slug         │
├──────────────────┼──────────────┼──────────────┤
│ 47cda535-...     │ liberum      │ liberuum     │
│ e5f6g7h8-...     │ Vetra        │ vetra        │
└──────────────────┴──────────────┴──────────────┘

staging> overview                  # Guide topics work without "guide" prefix
staging> config use local          # Profile switch auto-refreshes client
local> exit
```

---

### 18. Shell Completions

```
switchboard completions [bash|zsh|fish]    # Generate completions (auto-detects shell from $SHELL)
switchboard completions --install          # Auto-install into shell config file
```

Generated by `clap_complete`. Covers all commands, subcommands, and flags.

---

### 19. Self-Update

```
switchboard update                         # Update to latest version (shows changelog)
switchboard update --check                 # Check for updates without installing
```

The update command:

1. Queries the GitHub Releases API for newer versions
2. Shows a changelog covering every version between current and latest
3. Asks for confirmation before proceeding
4. Downloads and replaces the running binary atomically
5. Requests sudo if installed in a system directory (e.g. `/usr/local/bin`)

Supports macOS ARM64 (Apple Silicon) and Linux x86_64.

---

### 20. Built-in Guide

```
switchboard guide <topic>
```

15 built-in documentation topics:

| Topic | Description |
|-------|-------------|
| `overview` | Getting started |
| `config` | Profiles and configuration |
| `drives` | Working with drives |
| `docs` | Documents, mutations, models |
| `import-export` | .phd file format |
| `auth` | Authentication |
| `permissions` | Permissions system |
| `watch` | WebSocket subscriptions |
| `jobs` | Async job tracking |
| `sync` | Sync channels |
| `interactive` | REPL mode |
| `output` | Formatting and scripting |
| `graphql` | Raw GraphQL patterns |
| `visualize` | Visualization formats |
| `commands` | All commands at a glance |

---

## Rust Architecture

```
switchboard-cli/
├── Cargo.toml
├── src/
│   ├── main.rs                     # Entry point, clap setup, TTY detection
│   ├── cli/
│   │   ├── mod.rs                  # Cli struct, Commands enum, dispatch() fn
│   │   ├── helpers.rs              # setup(), resolve_drive_id(), build_client()
│   │   ├── init.rs                 # First-run wizard + introspection
│   │   ├── config.rs               # Profile management (list/show/use/remove)
│   │   ├── introspect.rs           # Schema discovery + caching
│   │   ├── drives.rs               # Drive CRUD (supports multi-delete)
│   │   ├── docs.rs                 # Document CRUD (supports multi-delete)
│   │   ├── models.rs               # Model inspection (from cache)
│   │   ├── ops.rs                  # Operations history (with input display)
│   │   ├── mutate.rs               # Model-specific document mutations
│   │   ├── field_editor.rs         # Field-by-field mutation editor (introspection + prompting)
│   │   ├── analytics.rs            # Analytics queries (metrics, dimensions, currencies, series)
│   │   ├── import_export.rs        # .phd file import/export with filters
│   │   ├── auth.rs                 # Authentication commands
│   │   ├── access.rs               # Document/operation permissions (stub)
│   │   ├── groups.rs               # User group management (stub)
│   │   ├── query.rs                # Raw GraphQL execution
│   │   ├── schema.rs               # Full schema dump
│   │   ├── watch.rs                # WebSocket subscriptions (with --exec)
│   │   ├── jobs.rs                 # Async job tracking
│   │   ├── sync.rs                 # Sync channel operations
│   │   ├── interactive.rs          # REPL mode (rustyline, full CLI parity via clap dispatch)
│   │   ├── visualize.rs            # Visualize all drives/docs as diagrams
│   │   ├── guide.rs                # Built-in documentation (15 topics)
│   │   ├── update.rs               # Self-update (GitHub Releases + binary swap)
│   │   └── completions.rs          # Shell completion generation
│   ├── graphql/
│   │   ├── client.rs               # GraphQLClient — HTTP POST + auth header injection
│   │   ├── introspection.rs        # Schema introspection + caching
│   │   └── websocket.rs            # WebSocket client (graphql-transport-ws protocol)
│   ├── config/
│   │   └── profiles.rs             # TOML profile management
│   ├── phd/
│   │   ├── reader.rs               # Read .phd ZIP archives
│   │   ├── writer.rs               # Create .phd ZIP archives
│   │   └── types.rs                # PhdHeader, PhdOperations structs
│   └── output/
│       ├── table.rs                # Table formatter (comfy-table)
│       ├── json.rs                 # JSON formatter (serde_json)
│       ├── tree.rs                 # DriveTree shared data model for all renderers
│       ├── svg.rs                  # SVG renderer (Powerhouse-themed)
│       ├── png.rs                  # PNG rasterizer (resvg wrapper)
│       └── mermaid.rs              # Mermaid flowchart renderer
└── tests/
    └── cli_integration.rs          # Integration tests (requires running GraphQL API)
```

---

## Rust Crate Dependencies

| Crate | Purpose |
|-------|---------|
| `clap` + `clap_complete` | CLI parsing, subcommands, shell completions |
| `reqwest` + `rustls` | HTTP client + TLS for GraphQL requests |
| `tokio` | Async runtime |
| `serde` + `serde_json` | JSON serialization/deserialization |
| `toml` | Profile config file parsing |
| `dialoguer` | Interactive prompts (init wizard, drive create, doc create, field editor) |
| `comfy-table` | Table output formatting |
| `colored` | Terminal colors |
| `zip` | .phd file reading and writing (ZIP format) |
| `tokio-tungstenite` | WebSocket for subscriptions |
| `dirs` | Cross-platform config directory (~/.switchboard/) |
| `rustyline` | REPL line editing, history, tab completion |
| `resvg` + `usvg` + `tiny-skia` | SVG → PNG rasterization |

---

## Distribution

| Channel | Command |
|---------|---------|
| Install script | `curl -fsSL .../install.sh \| bash` |
| GitHub Releases | Download binary for linux-x64 or darwin-arm64 |
| Cargo | `cargo install switchboard-cli` (when published) |
| Homebrew | `brew install powerhouse/tap/switchboard` (when published) |

CI builds for Linux x86_64 and macOS ARM64 (Apple Silicon).
Binary size target: **~8-12MB** (static binary).

---

## Testing Strategy

| Layer | What | Against |
|-------|------|---------|
| Unit | CLI arg parsing, output formatting, .phd ZIP read/write, slug→UUID resolution | Mocked |
| Integration (read) | drives list, docs list, docs get, models list, ops list | staging API |
| Integration (write) | drives create/delete, docs create/delete, mutations, import/export | local server (localhost:4001) |
| .phd round-trip | Export doc → import doc → verify state matches | local server |

---

## Example Session

```bash
# First time setup
$ switchboard init
> Paste your Switchboard GraphQL URL: https://switchboard-staging.powerhouse.xyz/graphql
> Profile name [staging]: staging
✓ Connected. Introspecting schema...
✓ 19 document models discovered
✓ 27 drives found
✓ Profile "staging" saved as default

# Discover what's available on this instance
$ switchboard models list
┌───────────────────────────────────┬─────────────────────┐
│ Type                              │ Prefix              │
├───────────────────────────────────┼─────────────────────┤
│ powerhouse/invoice                │ Invoice             │
│ powerhouse/builder-profile        │ BuilderProfile      │
│ powerhouse/resource-template      │ ResourceTemplate    │
│ powerhouse/scope-of-work          │ ScopeOfWork         │
│ ...                               │ ...                 │
└───────────────────────────────────┴─────────────────────┘

# List drives
$ switchboard drives list
┌────────────────────────┬────────────────────────┬────────────────────────┐
│ ID                     │ Name                   │ Slug                   │
├────────────────────────┼────────────────────────┼────────────────────────┤
│ powerhouse             │ Powerhouse             │ powerhouse             │
│ builders               │ builders               │ builders               │
│ mesa                   │ Mesa                   │ mesa                   │
└────────────────────────┴────────────────────────┴────────────────────────┘

# Browse documents in a drive
$ switchboard docs tree builders
builders/
├── Acaldas (powerhouse/builder-profile)
├── 📁 Core Contributors/
│   ├── Alice (powerhouse/builder-profile)
│   └── Bob (powerhouse/builder-profile)
└── ...

# Get a document's full state
$ switchboard docs get Acaldas --drive builders --state --format json | jq '.state.name'
"Acaldas"

# Create a document (scripted)
$ switchboard docs create --type powerhouse/invoice --name "Q1 Invoice" --drive my-drive --format json
[{"id": "41d2cae7-...", "name": "Q1 Invoice", ...}]

# Mutate a document
$ switchboard docs mutate 41d2cae7-... --op editInvoice --input '{"amount": 2000}' --format json

# Export a whole drive as .phd files
$ switchboard export drive builders --out ./backup/
  Downloading 15 documents...
✓ 15 documents saved to ./backup/builders/

# Incremental export — only recent operations
$ switchboard export drive builders --out ./incremental/ --since-revision 100

# Import .phd files into another instance
$ switchboard -p local import ./backup/builders/*.phd --drive my-local-drive
  Importing 15 documents...
✓ 15 documents imported, all states verified

# Apply raw actions and wait
$ switchboard docs apply abc123 --file actions.json --wait --format json

# Watch for changes and react
$ switchboard watch docs --drive my-drive --exec './on-change.sh' --format json

# Pipe-friendly
$ switchboard docs list --drive builders --format json | jq '.[].id'
"92a6e064-..."
"03df64d8-..."

# Analytics
$ switchboard analytics series --start 2026-01-01 --end 2026-12-31 --granularity MONTHLY --format json
```
