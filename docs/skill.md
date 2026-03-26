# Switchboard CLI — AI Agent Skill

You are interacting with a Switchboard instance via the `switchboard` CLI. This document is a complete reference for every command, flag, and workflow. Read it fully before starting any task.

## Rules

1. **Always use `--format json`** on every command. Parse JSON output programmatically.
2. **Always use `--input-file`** for mutations with complex values (schemas, multiline strings). Shell escaping will corrupt `--input` values containing `\n`, `!`, `{}`.
3. **Always verify your work.** A mutation returning 200 does not mean success. You must confirm the document exists in the right drive and has the correct state.
4. **Introspect before guessing.** If you don't know a field name, query `__type`. Field names are not always obvious (e.g., `setAuthorName` expects `authorName`, not `name`).
5. **Schemas use GraphQL SDL**, not JSON Schema. If you write `{"type": "object", "properties": {...}}`, the document model will be malformed.
6. **Name resolution**: Most commands accept UUIDs, slugs, or human-readable names. The CLI resolves them automatically. Use `--drive` to narrow ambiguous name lookups.

## Global Flags

These apply to every command:

```
--format <FORMAT>     Output format: table, json, raw, svg, png, mermaid
                      (Default: table for TTY, json for pipes)
-p, --profile <NAME>  Use a specific profile instead of the default
--quiet               Suppress extra output
--no-color            Disable colored output
-i                    Launch interactive REPL mode
```

---

## Complete Command Reference

### Connection & Setup

```bash
switchboard init                               # Interactive first-run wizard (URL, profile, token, introspect)
switchboard ping                               # Health check
switchboard info --format json                 # Instance info (drive count, model count)
switchboard introspect                         # Re-discover schema (includes DocumentDrive; auto-runs on model miss)
switchboard schema --format json               # Dump the full GraphQL schema
switchboard update                             # Update CLI to latest version
switchboard update --check                     # Check for updates without installing
```

### Profiles

```bash
switchboard config list --format json          # List all profiles
switchboard config show --format json          # Show the active profile
switchboard config use <name>                  # Switch active profile
switchboard config remove <name>               # Remove a profile
```

### Authentication

```bash
switchboard auth login                         # Authenticate (prompts for token)
switchboard auth login --token <JWT>           # Authenticate with a token directly
switchboard auth logout                        # Remove token from current profile
switchboard auth status --format json          # Show auth status
switchboard auth token                         # Print the raw bearer token
```

### Drives

```bash
switchboard drives list --format json                       # List all drives
switchboard drives get <id|slug|name> --format json         # Get drive details
switchboard drives get <id> --format svg --out diagram.svg  # Visual diagram
switchboard drives create --name <name> --format json       # Create a drive
switchboard drives create --name <name> --icon <url> --preferred-editor <editor>
switchboard drives delete <id> [<id>...] -y                 # Delete one or more drives
switchboard drives check <id|slug|name>                      # Scan for ghost nodes (orphan file refs)
switchboard drives fix <id|slug|name> -y                     # Remove ghost nodes from a drive
```

### Documents — Read

```bash
switchboard docs list --format json                         # List docs across all drives
switchboard docs list --drive <slug> --format json          # List docs in a specific drive
switchboard docs list --drive <slug> -t <type> --format json  # Filter by document type
switchboard docs list --drive <slug> --format svg --out tree.svg  # Visual tree

switchboard docs get <id|name> --format json                # Get document metadata
switchboard docs get <id|name> --drive <slug> --format json # Narrow by drive
switchboard docs get <id|name> --state --format json        # Include full document state

switchboard docs tree --format json                         # File tree (all drives)
switchboard docs tree <drive> --format json                 # File tree (one drive)

switchboard docs parents <id|name> --format json            # Parent documents
```

### Documents — Write

```bash
switchboard docs create --type <type> --name <name> --drive <drive> --format json
switchboard docs rename <id|name> <new-name>
switchboard docs delete <id|name> [<id>...] -y              # Delete one or more documents

switchboard docs add-to <parent> <id> [<id>...]             # Add doc to a drive (DocumentDrive addFile)
switchboard docs remove-from <parent> <id> [<id>...]        # Unlink doc from a drive (DocumentDrive deleteNode)
switchboard docs move <id> [<id>...] --from <a> --to <b>    # Cross-drive move (deleteNode + addFile)
```

> **Note**: `add-to`, `remove-from`, and `move` use DocumentDrive model mutations (`addFile`, `deleteNode`) internally. The top-level `addChildren`/`removeChildren`/`moveChildren` GraphQL mutations exist but are no-ops on current API versions.

### Documents — Mutate

```bash
# Interactive mutation (picks operation and fields interactively)
switchboard docs mutate <doc_id|name>

# Scripted mutation (non-interactive)
switchboard docs mutate <doc_id|name> --op <operation> --input '<json>' --format json
switchboard docs mutate <doc_id|name> --op <operation> --input-file <path> --format json

# Narrow document lookup by drive
switchboard docs mutate <doc_id|name> --drive <slug> --op <operation> --input '<json>' --format json
```

### Documents — Apply Raw Actions

```bash
# Apply raw actions (async, returns job ID)
# timestampUtcMs is auto-injected as ISO-8601 into each action if missing
switchboard docs apply <id> --actions '<json_array>' --format json
switchboard docs apply <id> --file actions.json --format json
switchboard docs apply <id> --file - --format json          # Read from stdin

# Apply and wait for completion
switchboard docs apply <id> --file actions.json --wait --format json
```

### Models — Discovery

```bash
switchboard models list --format json                       # All available document models
switchboard models get <type|prefix> --format json          # Operations for a model
# Example: switchboard models get powerhouse/invoice --format json
# Example: switchboard models get Invoice --format json
```

### Operation History

```bash
switchboard ops <doc_id|name> --format json                 # Full operation history
switchboard ops <doc_id|name> --drive <slug> --format json  # Specify drive for name resolution
switchboard ops <doc_id|name> --skip 10 --first 5 --format json  # Paginate
```

### Raw GraphQL

```bash
switchboard query '<graphql>' --format json
switchboard query '<mutation>' --variables '<json>' --format json
switchboard query --file query.graphql --format json
switchboard query --file query.graphql --variables '{"id": "..."}' --format json

# Introspect an unknown input type
switchboard query '{ __type(name: "<InputType>") { inputFields { name type { name kind ofType { name } } } } }' --format json
```

### Export

```bash
# Export everything
switchboard export all --format json
switchboard export all -o ./backup/ --format json

# Export a single document as .phd
switchboard export doc <doc_id> --drive <slug> --format json
switchboard export doc <doc_id> --drive <slug> -o output.phd --format json

# Export all documents in a drive
switchboard export drive <drive_slug> --format json
switchboard export drive <drive_slug> -o ./drive-backup/ --format json

# Export with filters (apply to all export commands)
switchboard export all --action-types SET_NAME,ADD_ITEM --format json
switchboard export all --since-revision 50 --format json
switchboard export all --from 2026-01-01T00:00:00Z --to 2026-03-01T00:00:00Z --format json
```

**Filter flags** (available on `export all`, `export doc`, `export drive`):

| Flag | Description |
|------|-------------|
| `--action-types <TYPES>` | Comma-separated action types to include |
| `--since-revision <N>` | Only operations since this revision index |
| `--from <ISO-8601>` | Only operations from this timestamp |
| `--to <ISO-8601>` | Only operations up to this timestamp |

### Import

```bash
switchboard import <file.phd> --drive <slug>                # Import one .phd file
switchboard import a.phd b.phd c.phd --drive <slug>         # Import multiple .phd files
```

### Real-Time Subscriptions (WebSocket)

```bash
# Watch all document changes
switchboard watch docs --format json

# Watch filtered by drive, type, or document
switchboard watch docs --drive <slug> --format json
switchboard watch docs -t <type> --format json
switchboard watch docs --doc <id> --format json

# Execute a command for each event (receives JSON on stdin)
switchboard watch docs --exec 'jq .documentId' --format json
switchboard watch docs --drive <slug> --exec './on-change.sh' --format json

# Watch a specific job's status updates
switchboard watch job <job_id> --format json
```

### Async Jobs

```bash
switchboard jobs status <job_id> --format json              # Get job status
switchboard jobs wait <job_id> --format json                # Block until job completes
switchboard jobs wait <job_id> --timeout 600 --format json  # Custom timeout
switchboard jobs watch <job_id> --format json               # Stream updates via WebSocket
```

### Sync Channels

```bash
switchboard sync touch '<channel_json>'                     # Create/update a sync channel
switchboard sync touch @channel.json                        # Read from file (prefix with @)

switchboard sync push '<envelopes_json>'                    # Push sync envelopes
switchboard sync push @envelopes.json                       # Read from file

switchboard sync poll <channel_id> --format json            # Poll for envelopes
switchboard sync poll <channel_id> --ack 5 --format json    # Acknowledge up to sequence 5
switchboard sync poll <channel_id> --latest 10 --format json
```

### Analytics

```bash
switchboard analytics metrics --format json                 # List available metrics
switchboard analytics dimensions --format json              # List dimensions and values
switchboard analytics currencies --format json              # List available currencies

# Query time series
switchboard analytics series \
  --start 2026-01-01 --end 2026-12-31 \
  --granularity MONTHLY \
  --metrics revenue,expenses \
  --currency USD \
  --format json
```

**Granularity options**: `HOURLY`, `DAILY`, `WEEKLY`, `MONTHLY`, `ANNUALLY`, `TOTAL`

### Visualization

```bash
switchboard visualize --format svg -o all-drives.svg        # SVG diagram of all drives/docs
switchboard visualize --format png -o all-drives.png        # PNG rasterized diagram
switchboard visualize --format mermaid                      # Mermaid flowchart to stdout
switchboard visualize --format json                         # JSON tree structure
```

Visual formats (`svg`, `png`, `mermaid`) are also supported on:
- `drives get <id> --format svg --out drive.svg`
- `docs list --drive <slug> --format svg --out docs.svg`

### Shell Completions

```bash
switchboard completions                                     # Print completions (auto-detect shell)
switchboard completions bash                                # Generate for bash
switchboard completions zsh                                 # Generate for zsh
switchboard completions fish                                # Generate for fish
switchboard completions --install                           # Install into shell config file
```

### Built-in Guide

```bash
switchboard guide                                           # List all topics
switchboard guide overview                                  # Getting started
switchboard guide config                                    # Profile management
switchboard guide drives                                    # Working with drives
switchboard guide docs                                      # Document CRUD and mutations
switchboard guide import-export                             # .phd file import/export
switchboard guide auth                                      # Authentication
switchboard guide permissions                               # Permissions system
switchboard guide watch                                     # WebSocket subscriptions
switchboard guide jobs                                      # Async job tracking
switchboard guide sync                                      # Sync channels
switchboard guide interactive                               # REPL mode
switchboard guide output                                    # Output formatting and piping
switchboard guide graphql                                   # Raw GraphQL patterns
switchboard guide visualize                                 # Diagram generation
switchboard guide commands                                  # All commands at a glance
```

### Interactive REPL

```bash
switchboard -i                                              # Launch REPL
switchboard interactive                                     # Same thing
```

The REPL has **full CLI parity** — every command works inside it. Tab completion for drive slugs, model types, guide topics, and command names. History persists across sessions.

---

## Workflow: Creating a Document Model

This is the most common multi-step workflow. Follow every step.

### Step 1: Create the document

```bash
switchboard docs create --type 'powerhouse/document-model' --name '<Name>' --drive <DRIVE_SLUG> --format json
```

**Save the returned `id`.** Every subsequent command uses it.

### Step 2: Set metadata

```bash
switchboard docs mutate $DOC_ID --op setModelName        --input '{"name": "<Name>"}'           --format json
switchboard docs mutate $DOC_ID --op setModelId           --input '{"id": "<org/model-name>"}'   --format json
switchboard docs mutate $DOC_ID --op setModelDescription  --input '{"description": "<text>"}'    --format json
switchboard docs mutate $DOC_ID --op setAuthorName        --input '{"authorName": "<name>"}'     --format json
switchboard docs mutate $DOC_ID --op setAuthorWebsite     --input '{"authorWebsite": "<url>"}'   --format json
```

### Step 3: Set state schema (GraphQL SDL)

Write to a file, then apply:

```bash
cat > /tmp/schema.json <<'EOF'
{
  "scope": "global",
  "schema": "type MyModelState {\n  items: [Item!]!\n  count: Int!\n}\n\ntype Item {\n  id: String!\n  title: String!\n  status: ItemStatus!\n}\n\nenum ItemStatus {\n  draft\n  active\n  archived\n}"
}
EOF

switchboard docs mutate $DOC_ID --op setStateSchema --input-file /tmp/schema.json --format json
```

The `schema` value is a JSON string containing GraphQL SDL with `\n` for newlines.

### Step 4: Set initial state

```bash
cat > /tmp/init.json <<'EOF'
{
  "scope": "global",
  "initialValue": "{\"items\": [], \"count\": 0}"
}
EOF

switchboard docs mutate $DOC_ID --op setInitialState --input-file /tmp/init.json --format json
```

`initialValue` is a JSON-encoded string matching the schema's default state.

### Step 5: Add module and operations

```bash
# Module
switchboard docs mutate $DOC_ID --op addModule \
  --input '{"id": "core", "name": "Core Operations", "description": "CRUD ops"}' --format json

# Operation (use --input-file because schema contains special chars)
cat > /tmp/op.json <<'EOF'
{
  "moduleId": "core",
  "id": "add-item",
  "name": "addItem",
  "description": "Create a new item",
  "schema": "input AddItemInput {\n  title: String!\n  status: ItemStatus\n}",
  "template": "",
  "reducer": "",
  "scope": "global"
}
EOF
switchboard docs mutate $DOC_ID --op addOperation --input-file /tmp/op.json --format json
```

Repeat for each operation.

### Step 6: Register in the drive

Creating a document does **not** automatically make it visible in the drive's file tree. You must register it:

```bash
switchboard docs mutate <DRIVE_ID> --op addFile \
  --input '{"id": "<DOC_ID>", "name": "<Name>", "documentType": "powerhouse/document-model"}' --format json
```

**If you skip this step, the document is orphaned and invisible to users.**

### Step 7: Verify (MANDATORY)

You must run all of these checks. Do not skip any.

```bash
# 1. Document is in the drive's file list
switchboard docs list --drive <DRIVE_SLUG> --format json
# → Parse the JSON array. Your DOC_ID MUST appear. If not: Step 6 was missed.

# 2. Parent relationship exists
switchboard docs parents $DOC_ID --format json
# → Must return the target drive. If empty: document is orphaned.

# 3. Document state is populated (use --state for full inspection)
switchboard docs get $DOC_ID --state --format json
# → Verify: name, documentType, state are all present.
# → In state.global: check name, id, description, author, specifications, modules, operations.
```

**If any check fails, fix it before reporting success.**

---

## Workflow: Batch Document Creation

Use a shell loop to create multiple documents from a list:

```bash
for name in "Invoice-001" "Invoice-002" "Invoice-003"; do
  switchboard docs create --type 'powerhouse/invoice' --name "$name" --drive my-drive --format json
done
```

Or create documents and mutate them in sequence:

```bash
DOC_ID=$(switchboard docs create --type 'powerhouse/invoice' --name 'New Invoice' --drive billing --format json | jq -r '.[0].id')
switchboard docs mutate "$DOC_ID" --op setInvoiceNumber --input '{"invoiceNumber": "INV-2026-001"}' --format json
```

---

## Workflow: Export, Transform, Import

Back up a drive, transform data, and import into another instance:

```bash
# Export all documents from a drive
switchboard export drive production-data -o ./backup/ --format json

# Export with filters (e.g., only recent changes)
switchboard export drive production-data --since-revision 100 -o ./recent/ --format json

# Export with date range
switchboard export all --from 2026-01-01T00:00:00Z --to 2026-03-01T00:00:00Z -o ./q1/ --format json

# Switch to target instance and import
switchboard -p target-instance import ./backup/*.phd --drive target-drive
```

---

## Workflow: Watch and React

Monitor changes and trigger automation:

```bash
# Stream changes as JSON (pipe to jq, log, or another tool)
switchboard watch docs --drive my-drive --format json | jq '.documentId'

# Execute a script on each change
switchboard watch docs --drive my-drive --exec './process-change.sh' --format json

# Watch a specific document type
switchboard watch docs -t 'powerhouse/invoice' --exec 'curl -X POST ...' --format json
```

---

## Workflow: Async Job Lifecycle

For long-running operations (e.g., `docs apply`):

```bash
# Apply actions and get a job ID
JOB_ID=$(switchboard docs apply $DOC_ID --file actions.json --format json | jq -r '.jobId')

# Option 1: Poll until complete
switchboard jobs wait "$JOB_ID" --timeout 120 --format json

# Option 2: Stream updates via WebSocket
switchboard jobs watch "$JOB_ID" --format json

# Option 3: Apply and wait in one command
switchboard docs apply $DOC_ID --file actions.json --wait --format json
```

---

## Input Field Reference

### Document Model mutations

| Operation | Input fields |
|-----------|-------------|
| `setModelName` | `name: String!` |
| `setModelId` | `id: String!` |
| `setModelDescription` | `description: String!` |
| `setAuthorName` | `authorName: String!` |
| `setAuthorWebsite` | `authorWebsite: String!` |
| `setStateSchema` | `scope: String!`, `schema: String!` |
| `setInitialState` | `scope: String!`, `initialValue: String!` |
| `addModule` | `id: ID!`, `name: String!`, `description: String` |
| `addOperation` | `moduleId: ID!`, `id: ID!`, `name: String!`, `schema: String`, `description: String`, `template: String`, `reducer: String`, `scope: String` |
| `setOperationSchema` | `id: ID!`, `schema: String` |

### Drive mutations (DocumentDrive)

| Operation | Input fields |
|-----------|-------------|
| `addFile` | `id: ID!`, `name: String!`, `documentType: String!`, `parentFolder: ID` |
| `addFolder` | `id: ID!`, `name: String!`, `parentFolder: ID` |
| `deleteNode` | `id: ID!` |
| `moveNode` | `srcFolder: ID!`, `targetParentFolder: ID` (moves a node within the same drive) |
| `setDriveName` | `name: String!` |

### Discovering unknown input types

```bash
switchboard query '{ __type(name: "<InputTypeName>") { inputFields { name type { name kind ofType { name } } } } }' --format json
```

---

## Common Failures

| Symptom | Cause | Fix |
|---------|-------|-----|
| Doc exists but not in drive | Missing `addFile` | Run Step 6 |
| `docs parents` returns `[]` | Document is orphaned | Run Step 6 |
| Visible in CLI but not in Connect | Not a file node in drive | Run Step 6 |
| `field not defined` error | Wrong field name in input | Introspect the `__type` |
| `invalid escape` on `--input` | Shell corrupted the JSON | Use `--input-file` instead |
| Schema shows JSON Schema | Wrong format | Rewrite as GraphQL SDL, re-apply |
| Enum value rejected | Passed as wrong case | Check `__type` enumValues for exact casing |
| `mutate` ignores operation | Used positional arg | Use `--op <operation>` flag |
| Name resolution fails | Ambiguous name across drives | Add `--drive <slug>` to narrow |
| Export missing operations | Filters too restrictive | Check `--since-revision` / `--from` / `--to` |
