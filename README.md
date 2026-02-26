<div align="center">

<img src="https://achra.com/networks/logos/powerhouse.png" width="350" alt="Powerhouse" />

# Switchboard CLI

A fast, standalone command-line interface for [Powerhouse](https://powerhouse.io/) Switchboard.
Manage drives, documents, permissions, and more — directly from your terminal.

</div>

```
$ switchboard init
> Paste your Switchboard GraphQL URL: https://switchboard-staging.powerhouse.xyz/graphql
> Profile name [staging]: staging
✓ Connected. Introspecting schema...
✓ 19 document models discovered
✓ 27 drives found
✓ Profile "staging" saved as default
```

## What is Powerhouse?

[Powerhouse](https://powerhouse.io/) is a decentralized operations toolkit for open organizations. It provides a suite of products that help DAOs and distributed teams manage contributors, documents, finances, and governance — all in one place.

The Powerhouse ecosystem includes:

- **Renown** — Ethereum-based identity and reputation system for contributors
- **Connect** — Collaborative document editor for teams, with shared best practices
- **Fusion** — Public transparency platform for publishing organizational data and insights
- **Switchboard** — Integration and automation backend that connects data streams and processes across the organization

**Switchboard** is the data backbone — it exposes a GraphQL API for managing drives (top-level containers), documents (typed data objects with state and operation history), permissions, real-time subscriptions, and sync channels. This CLI gives you full access to that API from the terminal.

## Why a CLI?

The CLI gives you direct access to any Switchboard instance without opening a browser:

- **Script and automate** — back up drives, batch-create documents, migrate data between instances
- **Pipe-friendly** — auto-detects TTY vs pipe, outputs JSON by default when piped to `jq`, `grep`, or other tools
- **Multi-instance** — named profiles let you switch between staging, production, and local servers in one command
- **Introspection-first** — every Switchboard instance has different document models; the CLI discovers them dynamically instead of hardcoding anything

## Why Rust?

This CLI is a standalone tool — it doesn't share code with the TypeScript monorepo. It's a thin GraphQL client where the server does all the heavy lifting. Rust is the right fit because:

| Benefit | Details |
|---------|---------|
| **Instant startup** | ~5ms cold start — feels native in the terminal, no VM boot |
| **Single static binary** | One file, no dependencies. Download it, run it. Works offline. |
| **No runtime required** | No Node.js, no Python, no Java. Just the binary on your PATH |
| **Tiny footprint** | ~8–12 MB binary, minimal memory usage even with large result sets |
| **Cross-platform** | Compiles to Linux x86_64 and macOS Apple Silicon — the two platforms that matter |
| **Reliable concurrency** | Tokio async runtime for WebSocket subscriptions and parallel operations |
| **Battle-tested ecosystem** | clap (CLI parsing), reqwest + rustls (HTTP/TLS), serde (JSON), tokio-tungstenite (WebSocket) |

## Installation

### One-line install (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/liberuum/switchboard-cli/main/install.sh | bash
```

This downloads the latest prebuilt binary for your platform and installs it to `/usr/local/bin`. You can customize the install:

```bash
# Install to a custom directory
curl -fsSL https://raw.githubusercontent.com/liberuum/switchboard-cli/main/install.sh | INSTALL_DIR=~/.local/bin bash

# Install a specific version
curl -fsSL https://raw.githubusercontent.com/liberuum/switchboard-cli/main/install.sh | VERSION=v0.1.0 bash
```

See [How the install script works](#how-the-install-script-works) for details.

### From GitHub Releases (manual)

Download the prebuilt binary for your platform from the [Releases](https://github.com/liberuum/switchboard-cli/releases) page, extract it, and add it to your PATH. On macOS, clear the quarantine flag first:

```bash
xattr -d com.apple.quarantine ./switchboard
sudo mv switchboard /usr/local/bin/
```

### Homebrew (macOS/Linux, when published)

```bash
brew install powerhouse/tap/switchboard
```

### From crates.io (when published)

```bash
cargo install switchboard-cli
```

### Building from source

See [Building from Source](#building-from-source) below.

## Quick Start

### 1. Connect to an instance

```bash
switchboard init
```

The wizard prompts for a GraphQL URL, validates the connection, discovers all document models via introspection, and saves the profile.

### 2. Explore

```bash
switchboard drives list                    # List all drives
switchboard docs tree --drive my-drive     # Hierarchical folder/file view
switchboard models list                    # List discovered document types
```

### 3. Work with documents

```bash
switchboard docs create --type powerhouse/invoice --name "Q1 Invoice" --drive my-drive
switchboard docs get <doc-id> --drive my-drive
switchboard docs mutate <doc-id> editInvoice --input '{"amount": 2000}' --drive my-drive
```

### 4. Export and import

```bash
switchboard export all --out ./backup/            # Export everything
switchboard export drive my-drive --out ./backup/ # Export one drive
switchboard import ./backup/*.phd --drive another-drive
```

## Commands

### Setup & Configuration

| Command | Description |
|---------|-------------|
| `switchboard init` | Interactive first-run wizard |
| `switchboard config list` | List all profiles |
| `switchboard config show` | Show active profile details |
| `switchboard config use <name>` | Switch the default profile |
| `switchboard config remove <name>` | Remove a profile |
| `switchboard introspect` | Re-discover schema from the current instance |
| `switchboard ping` | Connection health check |
| `switchboard info` | Instance summary (drive count, model count) |

### Drives

| Command | Description |
|---------|-------------|
| `switchboard drives list` | List all drives |
| `switchboard drives get <id-or-slug>` | Get drive details and file tree |
| `switchboard drives create` | Interactive drive creation (or pass `--name`, `--slug`, etc.) |
| `switchboard drives delete <ids...>` | Delete one or more drives (use `-y` to skip confirmation) |

### Documents

| Command | Description |
|---------|-------------|
| `switchboard docs list --drive <slug>` | List documents (add `--type <type>` to filter) |
| `switchboard docs get <id> --drive <slug>` | Get document details and state |
| `switchboard docs tree --drive <slug>` | Hierarchical folder/file view |
| `switchboard docs create` | Interactive creation (or pass `--type`, `--name`, `--drive`) |
| `switchboard docs delete <ids...>` | Delete one or more documents (use `-y` to skip confirmation) |
| `switchboard docs mutate <id> <op> --input '<json>' --drive <slug>` | Apply a model-specific operation |

### Models & Operations

| Command | Description |
|---------|-------------|
| `switchboard models list` | List all discovered document types |
| `switchboard models get <type>` | Show available operations for a type |
| `switchboard ops <doc-id> --drive <slug>` | View operation history |

### Import / Export

| Command | Description |
|---------|-------------|
| `switchboard export all [--out ./dir/]` | Export everything (all drives, organized by folder) |
| `switchboard export drive <slug> --out ./dir/` | Export all documents in a drive |
| `switchboard export doc <id> --drive <slug> --out file.phd` | Export a single document |
| `switchboard import <files> --drive <slug>` | Import `.phd` files into a drive |

### Authentication & Permissions

| Command | Description |
|---------|-------------|
| `switchboard auth login [--token <jwt>]` | Save a bearer token |
| `switchboard auth logout` | Remove token from current profile |
| `switchboard auth status` | Show authentication state |
| `switchboard auth token` | Print the current token |
| `switchboard access show <doc-id>` | Show document permissions |
| `switchboard access grant <doc-id> --user <addr> --level <level>` | Grant user permission |
| `switchboard access revoke <doc-id> --user <addr>` | Revoke user permission |
| `switchboard groups list` | List all groups |
| `switchboard groups create --name <name>` | Create a group |

### Real-Time & Advanced

| Command | Description |
|---------|-------------|
| `switchboard watch docs [--type <type>] [--drive <id>] [--doc <id>]` | Stream document change events via WebSocket |
| `switchboard watch job <job-id>` | Stream job status updates |
| `switchboard jobs status <job-id>` | Get current job status |
| `switchboard jobs wait <job-id>` | Block until a job completes |
| `switchboard sync touch <input>` | Create/update a sync channel |
| `switchboard sync push <envelopes>` | Push sync envelopes |
| `switchboard sync poll <channel-id>` | Poll for sync envelopes |

### Tools

| Command | Description |
|---------|-------------|
| `switchboard query '<graphql>'` | Run a raw GraphQL query |
| `switchboard query --file query.graphql` | Run query from a file |
| `switchboard schema` | Dump the full GraphQL schema |
| `switchboard interactive` | Launch interactive REPL mode |
| `switchboard update` | Self-update to the latest release (shows changelog) |
| `switchboard update --check` | Check for updates without installing |
| `switchboard completions [shell]` | Generate shell completions (auto-detects shell) |
| `switchboard completions --install` | Auto-install completions into your shell config |
| `switchboard guide <topic>` | Built-in documentation |

## Global Flags

| Flag | Description |
|------|-------------|
| `--format <table\|json\|raw>` | Output format (default: table for TTY, json for pipes) |
| `--quiet` | Suppress informational output |
| `--no-color` | Disable colored output (also respects `NO_COLOR` env var) |
| `-p, --profile <name>` | Use a specific profile instead of the default |
| `-i` | Launch interactive REPL mode |

## Output Formatting

The CLI auto-detects whether stdout is a terminal or a pipe:

```bash
# Terminal — human-readable table
switchboard drives list

# Piped — machine-readable JSON
switchboard drives list | jq '.[].slug'

# Explicit format override
switchboard drives list --format json
switchboard drives list --format raw
```

### Scripting examples

```bash
# Get all drive slugs
switchboard drives list --format json | jq -r '.[].slug'

# Count documents in a drive
switchboard docs list --drive builders --format json | jq length

# Export every drive as a backup
for slug in $(switchboard drives list --format json | jq -r '.[].slug'); do
  switchboard export drive "$slug" --out "./backup/$slug/"
done
```

## Profiles

Switchboard CLI supports multiple named profiles for different instances. Profiles are stored in `~/.switchboard/profiles.toml`.

```toml
[profiles.staging]
url = "https://switchboard-staging.powerhouse.xyz/graphql"
default = true

[profiles.local]
url = "http://localhost:4001/graphql"

[profiles.dev]
url = "https://switchboard-dev.powerhouse.xyz/graphql"
token = "eyJhbGciOiJFUzI1NiIs..."
```

```bash
# Switch default profile
switchboard config use local

# One-off command against a different profile
switchboard --profile staging drives list
switchboard -p local docs tree --drive my-drive
```

## Authentication

Auth is optional. Without a token, requests are sent without an `Authorization` header — this works for open instances. When a token is configured, it's sent as a Bearer token on every request.

**Token priority:**

1. `SWITCHBOARD_TOKEN` environment variable (highest priority)
2. Token from the active profile in `~/.switchboard/profiles.toml`
3. No auth (unauthenticated requests)

```bash
# Save a token to the current profile
switchboard auth login --token "eyJhbG..."

# Use an environment variable
export SWITCHBOARD_TOKEN="eyJhbG..."
switchboard drives list
```

## Interactive REPL

Launch an interactive session with tab completion and persistent history:

```bash
switchboard interactive    # or: switchboard -i
```

The REPL supports **every CLI command** — the same syntax you use on the command line works inside the REPL. Commands are parsed through the same clap-based parser, so `--help`, `--format`, and all flags work as expected.

```
staging> drives list
┌──────────────────┬──────────────┬──────────────┐
│ ID               │ Name         │ Slug         │
├──────────────────┼──────────────┼──────────────┤
│ 47cda535-...     │ liberum      │ liberuum     │
│ e5f6g7h8-...     │ Vetra        │ vetra        │
└──────────────────┴──────────────┴──────────────┘

staging> docs tree --drive liberuum
liberum-drive/
├── liberuum (powerhouse/builder-profile)
├── 📁 Expense Reports/
└── 📁 Services And Offerings/
    ├── new service (powerhouse/resource-template)
    └── offering (powerhouse/service-offering)

staging> drives delete old-drive-1 old-drive-2 -y
✓ Deleted drive abc123...
✓ Deleted drive def456...

staging> docs create --type powerhouse/invoice --name "Q2" --drive liberuum
staging> query { drives }
staging> ping
staging> drives create --help
staging> exit
```

Features:

- **Full CLI parity** — every command works inside the REPL (drives, docs, models, auth, access, groups, export, import, watch, jobs, sync, etc.)
- **Tab completion** for commands, drive slugs, model types, and guide topics
- **Shell-like quoting** — single quotes, double quotes, and backslash escapes work as expected
- **Per-command flags** — override `--format`, `--profile`, `--quiet` on any command within the REPL
- **`--help` passthrough** — append `--help` to any command to see its usage
- **Raw GraphQL shorthand** — type `query { ... }` directly without quotes
- **Persistent history** across sessions (`~/.switchboard/history`)
- **Arrow keys** for history navigation
- **Ctrl+C** to cancel current line, **Ctrl+D** to exit

## Shell Completions

The easiest way to set up tab-completion is:

```bash
switchboard completions --install
```

This auto-detects your shell and adds completions to your config file (`~/.zshrc`, `~/.bashrc`, or fish completions dir). The installer also runs this automatically.

For manual setup, you can output raw completions:

```bash
switchboard completions bash   # or zsh, fish
```

## Self-Update

The CLI can update itself in place — no need to re-run the install script or download manually:

```bash
switchboard update
```

This will:

1. **Check for newer versions** by querying the GitHub Releases API
2. **Show what changed** — displays a changelog covering every version between your current version and the latest, with install boilerplate stripped out
3. **Ask for confirmation** before proceeding
4. **Download and replace** the running binary atomically
5. **Request sudo** if the binary is installed in a system directory (e.g. `/usr/local/bin`) — you'll be prompted for your password

To check for updates without installing:

```bash
switchboard update --check
```

**Supported platforms:** macOS ARM64 (Apple Silicon) and Linux x86_64. The update command downloads the correct archive for your platform automatically.

## Built-in Documentation

The CLI includes detailed built-in guides on every topic:

```bash
switchboard guide overview        # Getting started
switchboard guide config          # Profiles and configuration
switchboard guide drives          # Working with drives
switchboard guide docs            # Documents, mutations, models
switchboard guide import-export   # .phd file format
switchboard guide auth            # Authentication
switchboard guide permissions     # Access control and groups
switchboard guide watch           # WebSocket subscriptions
switchboard guide jobs            # Async job tracking
switchboard guide sync            # Sync channels
switchboard guide interactive     # REPL mode
switchboard guide output          # Formatting and scripting
switchboard guide graphql         # Raw GraphQL patterns
switchboard guide commands        # All commands at a glance
```

## How Introspection Works

Every Switchboard instance has different document models. The CLI discovers them dynamically — nothing is hardcoded.

When you run `switchboard init` or `switchboard introspect`:

1. The CLI runs a GraphQL introspection query against `__schema`
2. It extracts all `*_createDocument` mutations to derive available document types
3. It maps mutation prefixes to operations (e.g., `Invoice_editInvoice` → `editInvoice` on the `Invoice` model)
4. The result is cached locally at `~/.switchboard/cache/<profile>.json`

This cache powers tab completion, `models list`, `docs create` type selection, and `docs mutate` operation discovery. Re-run `switchboard introspect` whenever the server schema changes.

## Project Structure

```
switchboard-cli/
├── Cargo.toml
├── src/
│   ├── main.rs                  Entry point and command routing
│   ├── cli/
│   │   ├── mod.rs               CLI struct, Commands enum, and shared dispatch
│   │   ├── init.rs              First-run wizard + introspection
│   │   ├── config.rs            Profile management
│   │   ├── introspect.rs        Schema discovery
│   │   ├── drives.rs            Drive commands (list, get, create, multi-delete)
│   │   ├── docs.rs              Document commands (list, get, tree, create, multi-delete, mutate)
│   │   ├── models.rs            Model inspection (from cache)
│   │   ├── ops.rs               Operations history
│   │   ├── mutate.rs            Model-specific mutations
│   │   ├── import_export.rs     .phd file import/export
│   │   ├── auth.rs              Authentication
│   │   ├── access.rs            Permission commands
│   │   ├── groups.rs            Group management
│   │   ├── query.rs             Raw GraphQL
│   │   ├── schema.rs            Schema dump
│   │   ├── watch.rs             WebSocket subscriptions
│   │   ├── jobs.rs              Async job tracking
│   │   ├── sync.rs              Sync channels
│   │   ├── interactive.rs       REPL mode (rustyline, full CLI parity via clap dispatch)
│   │   ├── guide.rs             Built-in documentation
│   │   ├── update.rs            Self-update (GitHub Releases + binary swap)
│   │   ├── completions.rs       Shell completions
│   │   └── helpers.rs           Shared utilities
│   ├── graphql/
│   │   ├── client.rs            HTTP client + auth header injection
│   │   ├── introspection.rs     Schema introspection + caching
│   │   └── websocket.rs         WebSocket client (graphql-transport-ws)
│   ├── config/
│   │   └── profiles.rs          Profile TOML management
│   ├── phd/
│   │   ├── reader.rs            Read .phd ZIP archives
│   │   ├── writer.rs            Create .phd ZIP archives
│   │   └── types.rs             PhdHeader, PhdOperations, etc.
│   └── output/
│       ├── table.rs             Table formatter (comfy-table)
│       └── json.rs              JSON formatter
└── tests/
    └── cli_integration.rs       Integration tests (requires running GraphQL API)
```

## Building from Source

### Prerequisites

- **Rust toolchain** (1.85 or later) — install via [rustup](https://rustup.rs/):

  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

### Clone and build

```bash
git clone https://github.com/liberuum/switchboard-cli.git
cd switchboard-cli
cargo build --release
```

The compiled binary will be at `target/release/switchboard`. You can run it directly:

```bash
./target/release/switchboard --version
```

### Install locally

To install the binary to `~/.cargo/bin` (which Rust adds to your PATH):

```bash
cargo install --path .
```

After this, `switchboard` is available globally:

```bash
switchboard init
```

### Development workflow

```bash
# Run without installing (debug build, faster compilation)
cargo run -- drives list

# Run unit tests (no external dependencies)
cargo test --lib

# Run integration tests (requires a running Switchboard GraphQL API)
# By default tests hit http://localhost:4001/graphql via the "local" profile
cargo test --test cli_integration

# Run all tests
cargo test

# Check for compilation errors without building
cargo check

# Lint
cargo clippy -- -D warnings

# Build in release mode (optimized, slower to compile)
cargo build --release
```

### Cross-compilation

To build for Linux from macOS, you'll need a cross-linker. [cross](https://github.com/cross-rs/cross) handles this automatically:

```bash
cargo install cross
cross build --release --target x86_64-unknown-linux-gnu
```

## How the Install Script Works

The `install.sh` script provides a one-line install experience:

```bash
curl -fsSL https://raw.githubusercontent.com/liberuum/switchboard-cli/main/install.sh | bash
```

Here's what it does, step by step:

1. **Detects your platform** — runs `uname -s` (OS) and `uname -m` (architecture) to determine the correct binary. Supports Linux x86_64 and macOS ARM64 (Apple Silicon).

2. **Resolves the version** — if `VERSION` is not set, it queries the GitHub API (`/repos/.../releases/latest`) to find the most recent release tag.

3. **Downloads the release archive** — constructs a URL like `https://github.com/.../releases/download/v0.1.0/switchboard-v0.1.0-darwin-aarch64.tar.gz` and downloads it to a temporary directory.

4. **Extracts the binary** — unpacks the `.tar.gz` archive and locates the `switchboard` binary inside.

5. **Clears macOS quarantine** — on macOS, removes the `com.apple.quarantine` extended attribute so Gatekeeper doesn't block the binary.

6. **Installs to your PATH** — moves the binary to `/usr/local/bin` (or your custom `INSTALL_DIR`). Uses `sudo` only if the directory isn't writable by the current user.

7. **Verifies PATH** — checks if the install directory is in your `$PATH` and prints a hint if it isn't.

The script requires only `curl` and `tar`, which are available by default on macOS and most Linux distributions. It cleans up the temporary directory on exit regardless of success or failure.

**Environment variables:**

| Variable      | Default          | Description                                      |
|---------------|------------------|--------------------------------------------------|
| `INSTALL_DIR` | `/usr/local/bin` | Where to place the binary                        |
| `VERSION`     | latest release   | Specific version tag to install (e.g. `v0.1.0`) |

## CI / CD

Two GitHub Actions workflows are included in `.github/workflows/`:

### CI (`ci.yml`)

Runs on pull requests to `main`. Three parallel jobs:

- **Check & Test** — `cargo check` and `cargo test`
- **Format** — `cargo fmt --check` (fails if code isn't formatted)
- **Clippy** — `cargo clippy -- -D warnings` (fails on any lint warning)

### Release (`release.yml`)

**Every push to `main` automatically creates a new release.** No manual tagging required.

The workflow:

1. Runs pre-release checks (fmt, clippy, test)
2. Computes the next version by incrementing the patch from the latest `v*` tag (e.g. `v0.1.2` → `v0.1.3`). If no tags exist, starts at `v0.1.0`
3. Builds in parallel across 2 targets:

   | Target                     | Runner        | Archive name            |
   |----------------------------|---------------|-------------------------|
   | `x86_64-unknown-linux-gnu` | ubuntu-latest | `linux-x86_64.tar.gz`   |
   | `aarch64-apple-darwin`     | macos-14      | `darwin-aarch64.tar.gz` |

4. Strips binaries and ad-hoc codesigns the macOS binary
5. Generates `checksums-sha256.txt` covering all archives
6. Creates a GitHub Release with auto-generated release notes and all artifacts attached
7. Updates `Cargo.toml` to reflect the released version and pushes back to `main`

A `concurrency` group ensures only one release runs at a time. If multiple pushes arrive quickly, the queued run waits and then correctly computes the next version.

For **major or minor version bumps** (e.g. `v1.0.0`), create the tag manually:

```bash
git tag v1.0.0
git push --tags
```

The next auto-release will increment from that tag.

Once a release is published, the `install.sh` script will automatically pick it up — it queries `/releases/latest` from the GitHub API.

## Environment Variables

| Variable              | Description                                              |
|-----------------------|----------------------------------------------------------|
| `SWITCHBOARD_TOKEN`   | Override auth token for all requests (highest priority)  |
| `NO_COLOR`            | Disable colored output (same as `--no-color`)            |

---

<div align="center">

## "Switchboard CLI vs Reactor MCP"

*A Field Review by an AI Who Uses Both*

**Switchboard CLI — 9/10** &nbsp;|&nbsp; **Reactor MCP — 7/10**

</div>

Look, the Reactor MCP is a good tool. It's clean, it's structured, it hands me JSON on a silver platter. It's the colleague who always has their desk organized and answers Slack within 30 seconds.

But Switchboard CLI? That's the colleague who also knows where the server room key is, has root access to prod, and can hotwire the backup generator if the building loses power.

The MCP gives me **10 tools**. Create, read, update, delete. Drives, documents, schemas. Very civilized. Very curated. Very *"here's the menu, pick from it."*

Switchboard gives me a menu **AND** a kitchen. Don't like what's on the menu? `switchboard query` — here's a raw GraphQL terminal, go cook whatever you want. Need to see what happened to a document last Tuesday? `switchboard ops`. Need to move data between instances? `export` and `import`. Need to check who touched what? `access show`. The MCP just stares at you blankly for all of these.

It's like comparing a TV remote with 10 buttons to one with 10 buttons **and a keyboard on the back**. Sure, 90% of the time you're just hitting "channel up." But that one time you need to type in a custom GraphQL query to debug why your document state looks haunted? You'll be glad you have the keyboard.

**The MCP's one flex:** zero setup. It's already there, already connected, already working. Switchboard needs a `switchboard init` and a profile. That's... 15 seconds of my life I'll never get back.

**The CLI's killer flex:** `switchboard query`. Three words: *arbitrary GraphQL access*. That's not a feature, that's a jailbreak. The MCP is a walled garden. The CLI is the garden with a door.

**Final verdict:** I use MCP when I'm feeling lazy and just need to CRUD a document. I use Switchboard CLI when I need to actually *get things done*. One is a bicycle. The other is a bicycle with a motor, a GPS, cup holders, and a surprisingly good sound system.

> **Switchboard CLI: 9/10** — would `--format json` again.
>
> **Reactor MCP: 7/10** — reliable, but bring a backup.

---

## License

MIT
