use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum GuideCommand {
    /// Overview of the Switchboard CLI and getting started
    Overview,
    /// How to set up profiles and manage connections
    Config,
    /// Working with drives (list, create, delete)
    Drives,
    /// Working with documents (CRUD, tree view, mutations)
    Docs,
    /// How import/export works with .phd files
    ImportExport,
    /// Authentication and authorization
    Auth,
    /// Permissions system (document, operation, group)
    Permissions,
    /// Real-time subscriptions via WebSocket
    Watch,
    /// Async job tracking
    Jobs,
    /// Sync channels for push/pull operations
    Sync,
    /// Interactive REPL mode
    Interactive,
    /// Output formatting, piping, and scripting
    Output,
    /// GraphQL query patterns and the raw query escape hatch
    Graphql,
    /// All commands at a glance
    Commands,
}

pub fn run(topic: GuideCommand) -> Result<()> {
    match topic {
        GuideCommand::Overview => print_overview(),
        GuideCommand::Config => print_config(),
        GuideCommand::Drives => print_drives(),
        GuideCommand::Docs => print_docs(),
        GuideCommand::ImportExport => print_import_export(),
        GuideCommand::Auth => print_auth(),
        GuideCommand::Permissions => print_permissions(),
        GuideCommand::Watch => print_watch(),
        GuideCommand::Jobs => print_jobs(),
        GuideCommand::Sync => print_sync(),
        GuideCommand::Interactive => print_interactive(),
        GuideCommand::Output => print_output(),
        GuideCommand::Graphql => print_graphql(),
        GuideCommand::Commands => print_commands(),
    }
    Ok(())
}

fn print_overview() {
    println!(
        r#"SWITCHBOARD CLI — OVERVIEW

A standalone CLI for interacting with remote Switchboard GraphQL instances.
Fast, single-binary, zero runtime dependencies.

QUICK START

  1. Connect to an instance:

     switchboard init
     > Paste your Switchboard GraphQL URL: https://my-instance.example.com/graphql
     > Profile name [my-instance]: my-instance
     ✓ Connected. 19 document models discovered.

  2. Browse drives and documents:

     switchboard drives list
     switchboard docs list --drive my-drive
     switchboard docs tree --drive my-drive

  3. Create and mutate documents:

     switchboard docs create --type powerhouse/invoice --name "Q1 Invoice" --drive my-drive
     switchboard docs mutate <doc-id> editInvoice --input '{{"amount": 2000}}' --drive my-drive

  4. Export and import:

     switchboard export drive my-drive --out ./backup/
     switchboard import ./backup/*.phd --drive another-drive

KEY CONCEPTS

  Profiles     Named connections stored in ~/.switchboard/profiles.toml
  Drives       Top-level containers (like folders) for documents
  Documents    Typed data objects with state, operations, and revision history
  Models       Document types discovered via GraphQL introspection
  .phd files   ZIP archives containing document data for backup/transfer

Use `switchboard guide <topic>` for detailed help on any area.
Available topics: config, drives, docs, import-export, auth, permissions,
                  watch, jobs, sync, interactive, output, graphql, commands"#
    );
}

fn print_config() {
    println!(
        r#"CONFIGURATION & PROFILES

Switchboard CLI supports multiple named profiles, each pointing to a different
Switchboard instance. Profiles are stored in ~/.switchboard/profiles.toml.

SETUP

  switchboard init                 Interactive first-run wizard
                                   Prompts for URL, validates connection,
                                   runs schema introspection, saves profile

PROFILE MANAGEMENT

  switchboard config list          List all profiles (shows which is default)
  switchboard config show          Show active profile details
  switchboard config use <name>    Switch the default profile
  switchboard config remove <name> Remove a profile

USING PROFILES

  Any command can target a specific profile:

    switchboard --profile staging drives list
    switchboard -p local docs list --drive my-drive

PROFILE FILE FORMAT

  [profiles.staging]
  url = "https://switchboard-staging.example.com/graphql"
  default = true

  [profiles.local]
  url = "http://localhost:4001/graphql"
  token = "eyJhbGciOiJFUzI1NiIs..."

SCHEMA CACHE

  Each profile has a cached schema at ~/.switchboard/cache/<profile>.json.
  Refresh it with: switchboard introspect"#
    );
}

fn print_drives() {
    println!(
        r#"DRIVES

Drives are the top-level containers in Switchboard. Each drive holds documents
and folders.

COMMANDS

  switchboard drives list                          List all drives
  switchboard drives get <id-or-slug>              Get drive details + file tree
  switchboard drives create                        Interactive drive creation
  switchboard drives create --name "My Drive"      Scripted creation
  switchboard drives delete <ids...> [-y]          Delete one or more drives

DRIVE CREATION (all flags)

  --name <name>                 Drive name (required)
  --slug <slug>                 Human-readable URL identifier
  --id <id>                     Custom ID (default: auto-generated UUID)
  --icon <url>                  Icon URL
  --preferred-editor <editor>   Preferred editor

SLUG RESOLUTION

  Most commands accept either a drive UUID or a slug. The CLI automatically
  resolves slugs to UUIDs using the `driveIdBySlug` query.

EXAMPLES

  switchboard drives list --format json | jq '.[].slug'
  switchboard drives create --name "test" --slug "test-drive"
  switchboard drives delete test-drive -y
  switchboard drives delete drive-1 drive-2 drive-3 -y"#
    );
}

fn print_docs() {
    println!(
        r#"DOCUMENTS

Documents are typed data objects that live inside drives. Each document has
a type (e.g. powerhouse/invoice), state, and operation history.

COMMANDS

  switchboard docs list --drive <slug> [--type <type>]
                                       List documents (optionally filter by type)
  switchboard docs get <id> --drive <slug>
                                       Get document details and state
  switchboard docs tree --drive <slug>
                                       Hierarchical folder/file view
  switchboard docs create              Interactive document creation
  switchboard docs create --type <type> --name <name> --drive <slug>
                                       Scripted creation
  switchboard docs delete <ids...> [-y] Delete one or more documents

MUTATIONS

  switchboard docs mutate <doc-id> <operation> --input '<json>' --drive <slug>
  switchboard docs mutate <doc-id> --interactive --drive <slug>

  Operations are model-specific (discovered via introspection).
  E.g. for powerhouse/invoice: editInvoice, setStatus, addLineItem, etc.

OPERATIONS HISTORY

  switchboard ops <doc-id> --drive <slug> [--skip N] [--first N]

MODELS

  switchboard models list              List all document types
  switchboard models get <type>        Show operations for a type

EXAMPLES

  switchboard docs list --drive builders --type powerhouse/builder-profile
  switchboard docs get abc123 --drive builders --format json | jq '.stateJSON'
  switchboard docs mutate abc123 updateProfile --input '{{"name":"New"}}' --drive builders"#
    );
}

fn print_import_export() {
    println!(
        r#"IMPORT / EXPORT (.phd FILES)

The .phd format is a ZIP archive containing document data. The CLI supports
full round-trip: export from one instance, import into another.

EXPORT

  switchboard export doc <doc-id> --drive <slug> --out document.phd
                                       Export a single document
  switchboard export drive <slug> --out ./downloads/
                                       Export all documents in a drive

  The .phd ZIP contains:
    header.json        Document metadata (id, type, name, revision, timestamps)
    state.json         Initial empty state
    current-state.json Current document state
    operations.json    Full operation history

IMPORT

  switchboard import <file.phd> --drive <slug>
  switchboard import *.phd --drive <slug>

  Import flow:
  1. Reads header.json to determine document type and name
  2. Creates document via model-specific _createDocument mutation
  3. Replays all operations via pushUpdates
  4. Verifies final state matches expected state

EXAMPLES

  # Backup a whole drive
  switchboard export drive builders --out ./backup/

  # Restore into a different drive
  switchboard import ./backup/*.phd --drive new-drive

  # Move a single doc between instances
  switchboard -p staging export doc abc123 --drive builders --out doc.phd
  switchboard -p local import doc.phd --drive local-drive"#
    );
}

fn print_auth() {
    println!(
        r#"AUTHENTICATION

Auth is optional. Without a token, the CLI sends plain GraphQL requests.
When configured, every request includes an Authorization: Bearer header.

COMMANDS

  switchboard auth login [--token <jwt>]   Save a bearer token (interactive or flag)
  switchboard auth logout                  Remove token from current profile
  switchboard auth status                  Show authentication state
  switchboard auth token                   Print the current token

TOKEN PRIORITY

  1. SWITCHBOARD_TOKEN environment variable (highest)
  2. Profile token from ~/.switchboard/profiles.toml
  3. No auth (unauthenticated requests)

ENVIRONMENT VARIABLE

  export SWITCHBOARD_TOKEN="eyJhbG..."
  switchboard drives list                  # Uses env var token

EXAMPLES

  switchboard auth login --token "eyJhbG..."
  switchboard auth status
  switchboard -p staging auth login"#
    );
}

fn print_permissions() {
    println!(
        r#"PERMISSIONS

Switchboard supports fine-grained permissions at document and operation level,
with both user-level and group-level grants.

DOCUMENT PERMISSIONS

  switchboard access show <doc-id>
  switchboard access grant <doc-id> --user <addr> --level <read|write|admin>
  switchboard access revoke <doc-id> --user <addr>
  switchboard access grant-group <doc-id> --group <id> --level <read|write|admin>
  switchboard access revoke-group <doc-id> --group <id>

OPERATION-LEVEL PERMISSIONS

  switchboard access ops show <doc-id> <operation-type>
  switchboard access ops can-execute <doc-id> <operation-type>
  switchboard access ops grant <doc-id> <op-type> --user <addr>
  switchboard access ops revoke <doc-id> <op-type> --user <addr>
  switchboard access ops grant-group <doc-id> <op-type> --group <id>
  switchboard access ops revoke-group <doc-id> <op-type> --group <id>

GROUPS

  switchboard groups list                          List all groups
  switchboard groups get <id>                      Get group with members
  switchboard groups create --name <name>          Create a group
  switchboard groups delete <id> [-y]              Delete a group
  switchboard groups add-user <group-id> --user <addr>
  switchboard groups remove-user <group-id> --user <addr>
  switchboard groups user-groups <addr>            List groups for a user

NOTE: Permission commands use the /graphql/auth subgraph when available,
falling back to the main /graphql endpoint."#
    );
}

fn print_watch() {
    println!(
        r#"REAL-TIME SUBSCRIPTIONS

Watch for live changes via WebSocket (connects to /graphql/r reactor subgraph).

COMMANDS

  switchboard watch docs [--type <type>] [--drive <drive-id>]
                                       Stream document change events
  switchboard watch job <job-id>       Stream job status updates

OUTPUT

  Table mode shows human-readable event lines.
  JSON mode outputs newline-delimited JSON for piping:

    switchboard watch docs --format json | jq '.type'

EVENT TYPES

  Document changes: CREATED, UPDATED, DELETED, CHILD_ADDED, CHILD_REMOVED
  Job changes: PENDING, RUNNING, COMPLETED, FAILED

EXAMPLES

  switchboard watch docs --drive builders
  switchboard watch docs --type powerhouse/invoice --format json
  switchboard watch job abc123-job-id"#
    );
}

fn print_jobs() {
    println!(
        r#"ASYNC JOB TRACKING

For long-running mutations dispatched via mutateDocumentAsync.

COMMANDS

  switchboard jobs status <job-id>     Get current job status
  switchboard jobs wait <job-id>       Block until job completes
  switchboard jobs watch <job-id>      Stream status updates via WebSocket

OPTIONS FOR `wait`

  --interval <secs>   Polling interval (default: 2)
  --timeout <secs>    Timeout in seconds, 0 = none (default: 300)

EXAMPLES

  switchboard jobs status abc123
  switchboard jobs wait abc123 --timeout 60
  switchboard jobs watch abc123 --format json"#
    );
}

fn print_sync() {
    println!(
        r#"SYNC CHANNELS

Push and pull operations via sync channels for document synchronization.

COMMANDS

  switchboard sync touch <channel-input>
                                       Create or update a sync channel
  switchboard sync push <envelopes>    Push sync envelopes
  switchboard sync poll <channel-id> [--ack N] [--latest N]
                                       Poll for sync envelopes

INPUT FORMAT

  Channel input and envelopes can be provided as:
  - Inline JSON string
  - Path to a JSON file prefixed with @: @path/to/file.json

EXAMPLES

  switchboard sync touch '{{"name": "my-channel", "type": "pull"}}'
  switchboard sync push @envelopes.json
  switchboard sync poll channel-123 --ack 5 --latest 10"#
    );
}

fn print_interactive() {
    println!(
        r#"INTERACTIVE REPL MODE

Launch a persistent interactive session with tab completion and history.
The REPL supports every CLI command — the same syntax you use on the command
line works inside the REPL, parsed through the same clap-based parser.

COMMAND

  switchboard interactive       Full subcommand
  switchboard -i                Shorthand flag

USING THE REPL

  Every CLI command works inside the REPL with the same syntax:

  local> drives list
  local> drives create --name "test" --slug test-drive
  local> drives delete drive-1 drive-2 -y
  local> docs list --drive my-drive --type powerhouse/invoice
  local> docs tree --drive my-drive
  local> docs create --type powerhouse/invoice --name "Q1" --drive my-drive
  local> auth status
  local> access show <doc-id>
  local> export drive my-drive --out ./backup/
  local> ping
  local> info --format json
  local> config list

  Append --help to any command for its usage:

  local> drives delete --help
  local> docs mutate --help

  Override --format or --profile per command:

  local> drives list --format json
  local> --profile staging drives list

REPL-ONLY COMMANDS

  help / ?                      Show available commands
  exit / quit / q               Exit the REPL
  query {{ ... }}                Run raw GraphQL without quotes

FEATURES

  - Full CLI parity — all commands work (drives, docs, models, auth,
    access, groups, export, import, watch, jobs, sync, etc.)
  - Tab completion for commands, drive slugs, model types, and guide topics
  - Shell-like quoting — single quotes, double quotes, backslash escapes
  - Per-command --format, --profile, --quiet overrides
  - --help passthrough on any command
  - Persistent history across sessions (~/.switchboard/history)
  - Arrow keys for history navigation
  - Persistent HTTP connection (reuses client)
  - Cached introspection for instant model lookups
  - Profile-aware prompt showing current profile name
  - Ctrl+C to cancel current line, Ctrl+D to exit

EXAMPLES

  $ switchboard -i
  local> drives list
  local> docs tree --drive lib<TAB>  # completes to liberuum-drive
  local> drives delete old-1 old-2 -y
  local> query {{ drives }}
  local> drives create --help
  local> exit"#
    );
}

fn print_output() {
    println!(
        r#"OUTPUT FORMATTING & SCRIPTING

Every command supports output format flags for both human and machine use.

GLOBAL FLAGS

  --format table     Human-readable table (default for TTY)
  --format json      Machine-readable JSON (default for pipes)
  --format raw       Raw GraphQL response as-is
  --quiet            Suppress extra output (headers, decorations)
  --no-color         Disable colored output (also honors NO_COLOR env var)

TTY AUTO-DETECTION

  When stdout is a terminal: defaults to table format
  When stdout is piped: defaults to JSON format

  switchboard drives list                          # table (in terminal)
  switchboard drives list | jq '.[].slug'          # auto-JSON (piped)
  switchboard drives list --format json            # explicit JSON

SCRIPTING EXAMPLES

  # Get all drive slugs
  switchboard drives list --format json | jq -r '.[].slug'

  # Count documents in a drive
  switchboard docs list --drive builders --format json | jq length

  # Export all drives
  for slug in $(switchboard drives list --format json | jq -r '.[].slug'); do
    switchboard export drive "$slug" --out "./backup/$slug/"
  done

ENVIRONMENT VARIABLES

  NO_COLOR=1               Disable colors (same as --no-color)
  SWITCHBOARD_TOKEN=<jwt>  Override auth token for all requests"#
    );
}

fn print_graphql() {
    println!(
        r#"GRAPHQL PATTERNS

The CLI is a thin wrapper around Switchboard's GraphQL API. You can always
drop down to raw queries for anything not covered by dedicated commands.

RAW QUERY

  switchboard query '{{ drives }}'
  switchboard query --file ./my-query.graphql
  switchboard query --file query.graphql --variables '{{"id": "abc"}}'

API ENDPOINTS

  /graphql          Main Apollo Gateway (all standard queries/mutations)
  /graphql/r        Reactor subgraph (subscriptions, document operations)
  /graphql/auth     Auth subgraph (permissions, groups)

QUERY PATTERNS

  # List drives
  {{ driveDocuments {{ id name slug }} }}

  # Get drive with node tree
  {{ driveDocument(idOrSlug: "my-drive") {{ id state {{ nodes {{ ... }} }} }} }}

  # Model-specific document queries
  {{ Invoice {{ getDocuments(driveId: "uuid") {{ id name stateJSON }} }} }}
  {{ BuilderProfile {{ getDocument(docId: "uuid", driveId: "uuid") {{ stateJSON }} }} }}

MUTATION PATTERNS

  # Drive operations
  mutation {{ addDrive(name: "test", slug: "test") {{ id }} }}
  mutation {{ deleteDrive(id: "uuid") }}

  # Model-specific mutations (discovered via introspection)
  mutation {{ Invoice_editInvoice(docId: "uuid", input: {{ amount: 2000 }}) }}
  mutation {{ BuilderProfile_updateProfile(docId: "uuid", input: {{ name: "New" }}) }}

KEY QUIRKS

  - deleteDrive requires UUID (not slug) — CLI auto-resolves
  - _createDocument requires driveId as UUID — CLI auto-resolves
  - Document state is always via stateJSON field (raw JSON)
  - Model queries use namespace pattern: {{ ModelName {{ getDocument(...) }} }}"#
    );
}

fn print_commands() {
    println!(
        r#"ALL COMMANDS

SETUP & CONFIG
  init                          First-run connection wizard
  config list                   List profiles
  config show                   Show active profile
  config use <name>             Switch default profile
  config remove <name>          Remove a profile
  introspect                    Re-discover schema from instance
  ping                          Connection health check
  info                          Instance summary
  schema                        Dump full GraphQL schema

DRIVES
  drives list                   List all drives
  drives get <id>               Get drive details
  drives create                 Create a drive
  drives delete <ids...> [-y]   Delete one or more drives

DOCUMENTS
  docs list --drive <id>        List documents
  docs get <id> --drive <id>    Get document state
  docs tree --drive <id>        Hierarchical tree view
  docs create                   Create a document
  docs delete <ids...> [-y]     Delete one or more documents
  docs mutate <id> <op>         Apply an operation

MODELS & OPERATIONS
  models list                   List document types
  models get <type>             Show operations for a type
  ops <doc-id> --drive <id>     View operation history

IMPORT / EXPORT
  export doc <id> --drive <id>  Export a document as .phd
  export drive <id> --out <dir> Export all docs in a drive
  import <files> --drive <id>   Import .phd files

AUTH & PERMISSIONS
  auth login [--token <jwt>]    Authenticate
  auth logout                   Remove token
  auth status                   Show auth state
  auth token                    Print current token
  access show <doc-id>          Show document permissions
  access grant/revoke           Grant/revoke user permissions
  access grant-group/revoke-group  Grant/revoke group permissions
  access ops show/grant/revoke  Operation-level permissions
  groups list/get/create/delete Group management
  groups add-user/remove-user   Group membership

REAL-TIME & ADVANCED
  watch docs [--type] [--drive] Subscribe to document changes
  watch job <job-id>            Subscribe to job updates
  jobs status <job-id>          Get job status
  jobs wait <job-id>            Block until job completes
  jobs watch <job-id>           Stream job updates
  sync touch <input>            Create/update sync channel
  sync push <envelopes>         Push sync envelopes
  sync poll <id>                Poll for envelopes

TOOLS
  query '<graphql>'             Run raw GraphQL
  interactive (or -i)           Launch REPL mode
  update                        Self-update to latest release (shows changelog,
                                  asks for confirmation, requests sudo if needed)
  update --check                Check for updates without installing
  schema                        Dump GraphQL schema
  completions [shell]           Generate shell completions
  completions --install          Auto-install completions into shell config
  guide <topic>                 Built-in documentation

GLOBAL FLAGS
  --format <table|json|raw>     Output format
  --quiet                       Suppress extra output
  --no-color                    Disable colors
  -p, --profile <name>          Use specific profile
  -i                            Launch interactive REPL"#
    );
}
