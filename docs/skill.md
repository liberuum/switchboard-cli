# Switchboard CLI — AI Agent Skill

You are interacting with a Switchboard instance via the `switchboard` CLI. This document tells you how to create, mutate, and verify documents. Read it fully before starting any task.

## Rules

1. **Always use `--format json`** on every command. Parse JSON output programmatically.
2. **Always use `--input-file`** for mutations with complex values (schemas, multiline strings). Shell escaping will corrupt `--input` values containing `\n`, `!`, `{}`.
3. **Always verify your work.** A mutation returning 200 does not mean success. You must confirm the document exists in the right drive and has the correct state.
4. **Introspect before guessing.** If you don't know a field name, query `__type`. Field names are not always obvious (e.g., `setAuthorName` expects `authorName`, not `name`).
5. **Schemas use GraphQL SDL**, not JSON Schema. If you write `{"type": "object", "properties": {...}}`, the document model will be malformed.

## Command Reference

```bash
# Connection
switchboard ping                              # Health check
switchboard info --format json                # Instance info (drive count, model count)

# Drives
switchboard drives list --format json         # List all drives
switchboard drives create <name>              # Create a drive (returns ID)
switchboard drives delete <id> [<id>...] -y   # Delete drives

# Documents — read
switchboard docs list --drive <slug> --format json    # List docs in a drive
switchboard docs get <id> --format json               # Get document metadata
switchboard docs tree <drive> --format json           # File tree
switchboard docs parents <id> --format json           # Parent documents
switchboard ops <id> --format json                    # Operation history

# Documents — write
switchboard docs create --type <type> --name <name> --drive <drive> --format json
switchboard docs mutate <id> <operation> --input '<json>' --format json
switchboard docs mutate <id> <operation> --input-file <path> --format json
switchboard docs rename <id> <new-name>
switchboard docs delete <id> [<id>...] -y
switchboard docs add-to <parent> <id>         # Add as child
switchboard docs remove-from <parent> <id>    # Remove from parent
switchboard docs move <id> --from <a> --to <b>

# Models — discovery
switchboard models list --format json         # All available models
switchboard models get <Prefix> --format json # Operations for a model

# Introspection
switchboard query '{ __type(name: "<InputType>") { inputFields { name type { name kind ofType { name } } } } }' --format json

# Raw GraphQL
switchboard query '<graphql>' --format json
switchboard query '<mutation>' --variables '<json>' --format json
```

## Creating a Document Model

This is the most important workflow. Follow every step.

### Step 1: Create the document

```bash
switchboard docs create --type 'powerhouse/document-model' --name '<Name>' --drive <DRIVE_SLUG> --format json
```

**Save the returned `id`.** Every subsequent command uses it.

### Step 2: Set metadata

```bash
switchboard docs mutate $DOC_ID setModelName        --input '{"name": "<Name>"}'           --format json
switchboard docs mutate $DOC_ID setModelId           --input '{"id": "<org/model-name>"}'   --format json
switchboard docs mutate $DOC_ID setModelDescription  --input '{"description": "<text>"}'    --format json
switchboard docs mutate $DOC_ID setAuthorName        --input '{"authorName": "<name>"}'     --format json
switchboard docs mutate $DOC_ID setAuthorWebsite     --input '{"authorWebsite": "<url>"}'   --format json
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

switchboard docs mutate $DOC_ID setStateSchema --input-file /tmp/schema.json --format json
```

The `schema` value is a JSON string containing GraphQL SDL with `\n` for newlines.

### Step 4: Set initial state

```bash
switchboard docs mutate $DOC_ID setInitialState \
  --input '{"scope": "global", "initialValue": "{\"items\": [], \"count\": 0}"}' --format json
```

`initialValue` is a JSON-encoded string matching the schema's default state.

### Step 5: Add module and operations

```bash
# Module
switchboard docs mutate $DOC_ID addModule \
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
switchboard docs mutate $DOC_ID addOperation --input-file /tmp/op.json --format json
```

Repeat for each operation.

### Step 6: Register in the drive

Creating a document does **not** automatically make it visible in the drive's file tree. You must register it:

```bash
switchboard docs mutate <DRIVE_ID> addFile \
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

# 3. Document state is populated
switchboard docs get $DOC_ID --format json
# → Verify: name, documentType, revisionsList are all present.

# 4. Full state inspection (metadata + schema + operations)
switchboard query '{ document(identifier: "<DOC_ID>") { document { name documentType state } } }' --format json
# → Verify in state.global:
#   - name, id, description, author.name, author.website
#   - specifications[0].state.global.schema contains your GraphQL SDL
#   - specifications[0].modules[0].operations lists your operations
```

**If any check fails, fix it before reporting success.**

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
| `moveNode` | `srcFolder: ID`, `targetFolder: ID`, `id: ID!` |
| `setDriveName` | `name: String!` |

### Discovering unknown input types

```bash
switchboard query '{ __type(name: "<InputTypeName>") { inputFields { name type { name kind ofType { name } } } } }' --format json
```

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
