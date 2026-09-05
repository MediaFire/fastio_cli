# fastio

A command-line interface for the [Fastio](https://fast.io) cloud storage platform. Built in Rust for speed, reliability, and cross-platform support.

## Installation

### npm (recommended)

```bash
npm install -g @vividengine/fastio-cli
```

Or run without installing:

```bash
npx @vividengine/fastio-cli --help
```

### Shell script

```bash
curl -fsSL https://raw.githubusercontent.com/MediaFire/fastio_cli/main/install.sh | sh
```

### Binary download

Pre-built binaries for macOS, Linux, and Windows are available on the [Releases](../../releases) page.

### From source

Requires Rust 1.85+ (edition 2024):

```bash
cargo install --path .
```

## Quick Start

```bash
# Log in (opens browser for PKCE authentication)
fastio auth login

# Or log in with email/password
fastio auth login --email user@example.com --password ****

# Check auth status
fastio auth status

# List organizations
fastio org list

# Create a workspace
fastio workspace create --org <org_id> "My Workspace"

# Upload a file
fastio upload file --workspace <workspace_id> ./document.pdf

# Download a file
fastio download file --workspace <workspace_id> <node_id> --output ./downloads/

# Ask the AI about your workspace
fastio ai chat --workspace <workspace_id> "What files do I have?"

# Log out
fastio auth logout
```

## Authentication

The CLI supports multiple authentication methods, checked in this order:

1. `--token` flag (one-off bearer token)
2. `FASTIO_TOKEN` environment variable
3. `FASTIO_API_KEY` environment variable
4. Stored credentials from `--profile` (or default profile)

### PKCE Browser Login (Recommended)

```bash
fastio auth login
```

Opens your browser for secure OAuth authentication. Tokens are stored locally and automatically refreshed.

### Email/Password Login

```bash
fastio auth login --email user@example.com --password ****
```

### API Key

```bash
# Create an API key
fastio auth api-key create --name "CI pipeline"

# Use it for subsequent commands
export FASTIO_API_KEY=your-key-here
fastio org list
```

### Two-Factor Authentication

```bash
# Check 2FA status
fastio auth 2fa status

# Enable 2FA
fastio auth 2fa setup --channel totp

# Verify 2FA code after login
fastio auth 2fa verify --code <CODE>
```

## Output Formats

All commands support `--format` to control output:

```bash
# Table format (default for terminals)
fastio org list

# Markdown format (default when piped; optimized for LLM consumers)
fastio org list | cat

# JSON format (for scripts that parse structured output)
fastio org list --format json

# CSV format
fastio org list --format csv

# Explicit markdown (GitHub-flavored, byte-equivalent to the server's ?output=markdown)
fastio org list --format markdown
# `--format md` is accepted as an alias

# Filter specific fields
fastio org list --fields name,id,description
```

Markdown replaced JSON as the non-TTY default on 2026-04-15. The
rendered output is **byte-equivalent** to what the Fastio API produces
for `?output=markdown`: a `**Result:** success|failure` preamble,
object-valued errors promoted to a leading `# Error` section, and each
remaining top-level key as an H1 section in insertion order. Arrays of
associative records render as GFM pipe tables (union of keys in
insertion order); scalar lists render as bulleted lists.

Note on escaping: bullet values and heading text are **not** escaped —
the renderer takes a light-touch approach matching the server contract,
because the output is meant to be read or rendered, not embedded into
other markdown. **Downstream consumers that render the output to HTML
MUST sanitize.** The renderer does strip Unicode bidi, zero-width, and
C0/C1 control characters as a Trojan-Source defense; table cells escape
`|`, `\`, `` ` ``, and newlines; HTML-like cell content is wrapped in
inline-code fences.

Pipelines that need machine-parseable output can opt back in with
`--format json`.

## Commands

### Authentication & User

| Group | Description |
|-------|-------------|
| `auth` | Login, logout, 2FA, API keys, OAuth sessions |
| `user` | User profile, search, assets, invitations |
| `configure` | CLI profiles and settings |

### Organizations & Workspaces

| Group | Description |
|-------|-------------|
| `org` | Org CRUD, billing, members, transfer tokens, discovery, assets |
| `workspace` | Workspace CRUD, search, limits, `jobs-status` (poll async jobs) |
| `member` | Workspace/share member management |
| `invitation` | List, accept, decline, delete invitations; `join` accepts or declines one by its email key |
| `dashboard` | Per-workspace dashboard card feed: get, dismiss, undismiss |

### Files & Storage

| Group | Description |
|-------|-------------|
| `search` | Unified search — one query across a workspace or share, grouped into `files` / `metadata` / `comments` buckets. Start here for any "find X" |
| `files` | List, create folders, move, copy, rename, delete, trash, versions, search (flat file list, `--workspace` or `--share`), `content` (a file's extracted text as addressable chunks — relevance `--query`, `--page`, `--chunk-from/--chunk-to`, or `--nodes` to score up to 10 files at once), lock |
| `upload` | File upload (chunked with progress), text upload, URL import, session management |
| `download` | File download (streaming with progress), folder ZIP, batch |
| `lock` | Acquire, check status, heartbeat, release file locks |
| `metadata` | Metadata extraction and search: `eligible`, `details`, `search`, and async single-file `extract` (SPENDS AI CREDITS — needs `--confirm-ai-spend`; `--wait` polls to a terminal state) |

### Shares & Collaboration

| Group | Description |
|-------|-------------|
| `share` | Share CRUD, files, members, password auth |
| `fileshare` | File Shares — durable single-file link shares (replaces the retired QuickShare): create/list/info/update/delete, grants, download/versions/preview, upload write-back, activity, ws-token |
| `comment` | Comments, replies, reactions, attachments |
| `event` | Activity events, search, polling |
| `preview` | File preview URLs and transforms |
| `asset` | Org/workspace/user asset management |

### AI

| Group | Description |
|-------|-------------|
| `ripley` | Fastio's delegated AI agent — ask, chat, history, message management, `summary`, `transactions` (`ai` is a hidden alias). Searching is not a Ripley verb — use `search` |

### Platform

| Group | Description |
|-------|-------------|
| `apps` | App listing, details, launching |
| `import` | Cloud import providers, identities, sources, jobs, writebacks |
| `intents` | Agent Intents — announce what you are doing so peers see it before they collide: `allocate`, `list`, `fill`, `get`, `release` |
| `mcp` | Start built-in MCP server for AI agents |
| `completions` | Generate shell completions (bash, zsh, fish, PowerShell) |

### Utilities

| Group | Description |
|-------|-------------|
| `how-to` | Grounded "how do I…" answers about Fastio itself (alias `howto`) |
| `id` | Inspect Fastio identifiers offline (no auth, no network): classify an id's entity type and family |
| `view` | Render a single note or `.md` file node in the terminal |
| `skill` | Print the built-in agent skill guide |
| `system` | API health checks: `ping`, `status` (no auth required) |

## MCP Server Mode

The CLI includes a built-in [Model Context Protocol](https://modelcontextprotocol.io) server for AI agent integration. Run it as a subprocess:

```bash
fastio mcp
```

This exposes the CLI's action-routed tool surface as MCP tools over stdio, compatible with Claude Desktop, VS Code, and other MCP-compatible clients. A few high-risk or local-byte operations (sign send/void, fileshare upload write-back and ws-token) remain CLI-only, and the sign tool appears only when E-Sign is enabled.

Tool responses are rendered as GitHub-flavored Markdown by default,
byte-equivalent to the Fastio API's `?output=markdown` output.
Markdown is substantially more token-efficient for LLMs than
pretty-printed JSON, so the MCP server uses it for every read and
status response.

```json
{
  "mcpServers": {
    "fastio": {
      "command": "fastio",
      "args": ["mcp"]
    }
  }
}
```

Filter which tools are available:

```bash
fastio mcp --tools auth,org,workspace,files,upload,download
```

Authentication and backend follow the standard CLI rules: the server honors the
global `--api-base`, `--token`, and `--profile` flags (and the `FASTIO_TOKEN` /
`FASTIO_API_KEY` env vars), so you can point it at a non-default backend or
profile — e.g. `fastio --profile staging mcp` or
`fastio --api-base https://api.example/current mcp`.

## Shell Completions

Generate shell completions for your shell:

```bash
# Bash
fastio completions bash > ~/.bash_completion.d/fastio

# Zsh
fastio completions zsh > ~/.zfunc/_fastio

# Fish
fastio completions fish > ~/.config/fish/completions/fastio.fish

# PowerShell
fastio completions powershell > _fastio.ps1
```

## Profiles

Manage multiple accounts with named profiles:

```bash
# Set up a profile interactively
fastio configure init

# Log in to a specific profile
fastio auth login --profile work

# Use a profile for a command
fastio org list --profile work

# Set default profile
fastio configure set-default work

# List all profiles
fastio configure list
```

## Global Options

| Flag | Description |
|------|-------------|
| `--format json\|table\|csv\|markdown` | Output format (markdown is the default when piped) |
| `--fields name,id,...` | Filter output fields |
| `--detail terse\|standard\|full` | Server-side response verbosity via `?output=<detail>`; when omitted the server returns its `full` shape |
| `--no-color` | Disable colored output |
| `--quiet` / `-q` | Suppress all output |
| `--verbose` / `-v` | Increase verbosity; repeatable (`-v` info, `-vv` debug, `-vvv` trace API calls) |
| `--profile <name>` | Use named profile |
| `--token <jwt>` | One-off bearer token |
| `--api-base <url>` | Override API base URL |
| `--version` / `-V` | Print version (root command only, e.g. `fastio --version`) |

## Configuration

Configuration files are stored in the platform config directory, under a `fastio-cli` folder: `$XDG_CONFIG_HOME/fastio-cli` (typically `~/.config/fastio-cli`) on Linux, `~/Library/Application Support/fastio-cli` on macOS, and `%APPDATA%\fastio-cli` on Windows:

| File | Purpose |
|------|---------|
| `config.json` | Profile settings and API base URL |
| `credentials.json` | Stored authentication tokens |

## License

Apache License 2.0. See [LICENSE](LICENSE) for details.
