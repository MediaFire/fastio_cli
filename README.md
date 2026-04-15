# fastio

A command-line interface for the [Fast.io](https://fast.io) cloud storage platform. Built in Rust for speed, reliability, and cross-platform support.

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
fastio auth 2fa verify <code>
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
rendered output is **byte-equivalent** to what the Fast.io API produces
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
| `workspace` | Workspace CRUD, metadata templates, notes, quickshares |
| `member` | Workspace/share member management |
| `invitation` | Accept, decline, delete invitations |

### Files & Storage

| Group | Description |
|-------|-------------|
| `files` | List, create folders, move, copy, rename, delete, trash, versions, search, lock |
| `upload` | File upload (chunked with progress), text upload, URL import, session management |
| `download` | File download (streaming with progress), folder ZIP, batch, quickshare |
| `lock` | Acquire, check, release file locks |

### Shares & Collaboration

| Group | Description |
|-------|-------------|
| `share` | Share CRUD, files, members, quickshares, password auth |
| `comment` | Comments, replies, reactions, linking |
| `event` | Activity events, search, polling |
| `preview` | File preview URLs and transforms |
| `asset` | Org/workspace/user asset management |

### AI & Workflow

| Group | Description |
|-------|-------------|
| `ai` | Chat, search, history, message management, summarize |
| `task` | Tasks, task lists, assignment, status changes |
| `worklog` | Worklog entries, interjections, acknowledgments |
| `approval` | Request, approve, reject approvals |
| `todo` | Todo items with toggle, bulk operations |

### Platform

| Group | Description |
|-------|-------------|
| `apps` | App listing, details, launching |
| `import` | Cloud import providers, identities, sources, jobs, writebacks |
| `mcp` | Start built-in MCP server for AI agents |
| `completions` | Generate shell completions (bash, zsh, fish, PowerShell) |

## MCP Server Mode

The CLI includes a built-in [Model Context Protocol](https://modelcontextprotocol.io) server for AI agent integration. Run it as a subprocess:

```bash
fastio mcp
```

This exposes all CLI functionality as MCP tools over stdio, compatible with Claude Desktop, VS Code, and other MCP-compatible clients.

Tool responses are rendered as GitHub-flavored Markdown by default,
byte-equivalent to the Fast.io API's `?output=markdown` output.
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
| `--no-color` | Disable colored output |
| `--quiet` / `-q` | Suppress all output |
| `--verbose` / `-v` | Enable debug logging |
| `--profile <name>` | Use named profile |
| `--token <jwt>` | One-off bearer token |
| `--api-base <url>` | Override API base URL |

## Configuration

Configuration files are stored in `~/.fastio/`:

| File | Purpose |
|------|---------|
| `config.json` | Profile settings and API base URL |
| `credentials.json` | Stored authentication tokens |

## License

Apache License 2.0. See [LICENSE](LICENSE) for details.
