# Fast.io CLI — Agent Guide

Use this guide when interacting with the Fast.io platform via the `fastio` CLI.

## Authentication

Authenticate before using any command. Two methods:

```bash
# Option 1: API key (best for agents/automation)
fastio --token YOUR_API_KEY auth check

# Option 2: PKCE browser login (interactive)
fastio auth login
# Opens a URL → sign in → paste the authorization code
```

For automation, pass `--token` on every command or set `FASTIO_TOKEN` env var:
```bash
export FASTIO_TOKEN=your_api_key
fastio org list
```

## Output Format

Always use `--format json` when parsing output programmatically:
```bash
fastio org list --format json
fastio files list --workspace ID --format json
```

Other formats: `table` (human-readable), `csv` (spreadsheets). Default is `table` for TTY, `json` for pipes.

Use `--quiet` to suppress all output (useful for write operations where you only care about exit code).

## Important: Intelligence (AI) Setting

Workspaces have an `intelligence` toggle. When **OFF** (default), the workspace is pure storage. When **ON**, documents are automatically indexed with embeddings for AI-powered search, chat, and summarization.

**Only enable intelligence when you need to query the data.** Ingestion is expensive (per-page cost). For workspaces used only for file storage, sharing, or uploads, leave intelligence OFF.

```bash
# Create a workspace WITHOUT intelligence (default, recommended for storage)
fastio workspace create --org ORG_ID --name "File Storage"

# Create a workspace WITH intelligence (only for AI/RAG use cases)
fastio workspace create --org ORG_ID --name "Knowledge Base" --intelligence true
```

## Core Workflows

### List organizations and workspaces
```bash
fastio org list --format json
fastio workspace list --org ORG_ID --format json
```

### Browse and manage files
```bash
# List root files
fastio files list --workspace WS_ID --format json

# List folder contents
fastio files list --workspace WS_ID --folder NODE_ID --format json

# Search
fastio files search --workspace WS_ID "query" --format json

# Recent files
fastio files recent --workspace WS_ID --format json

# Create folder
fastio files create-folder --workspace WS_ID --parent NODE_ID --name "My Folder"

# Delete (moves to trash)
fastio files delete --workspace WS_ID NODE_ID

# Permanently delete
fastio files purge --workspace WS_ID NODE_ID
```

### Upload files
```bash
# Upload a file
fastio upload file --workspace WS_ID ./path/to/file

# Upload to specific folder
fastio upload file --workspace WS_ID --folder FOLDER_NODE_ID ./path/to/file

# Upload text content directly
fastio upload text --workspace WS_ID --name "notes.txt" "File content here"

# Import from URL
fastio upload url --workspace WS_ID --url "https://example.com/file.pdf"
```

### Download files
```bash
# Download a file
fastio download file --workspace WS_ID --node-id NODE_ID

# Download folder as ZIP
fastio download folder --workspace WS_ID --node-id FOLDER_NODE_ID
```

### Share management (portals)
```bash
# List shares
fastio share list --format json

# Create a share
fastio share create "Share Name" --workspace WS_ID

# Get share details
fastio share info SHARE_ID --format json

# Guest auth (anonymous upload token)
fastio share guest-auth SHARE_ID
```

### AI queries

**Important:** AI search and chat require `intelligence` to be enabled on the workspace. Enabling intelligence triggers document embedding/indexing which incurs significant per-page ingestion costs. Only enable it on workspaces where you intend to query the data — do not enable it by default for storage-only workspaces.

```bash
# Enable intelligence on a workspace (only when needed for AI queries)
fastio workspace enable-workflow WS_ID

# Search workspace content (requires intelligence enabled)
fastio ai search --workspace WS_ID "your question" --format json

# Chat with workspace files
fastio ai chat --workspace WS_ID --message "Summarize the documents"

# Chat history
fastio ai history --workspace WS_ID --format json
```

### Members
```bash
fastio member list --workspace WS_ID --format json
fastio member add --workspace WS_ID user@example.com
fastio member remove --workspace WS_ID MEMBER_ID
```

## ID Formats

- **Organization IDs**: 19-digit numeric strings (e.g., `3867689418901071163`)
- **Workspace IDs**: 19-digit numeric strings (e.g., `4687730903718774523`)
- **Share IDs**: 19-digit numeric strings
- **Node IDs** (files/folders): Opaque alphanumeric with hyphens (e.g., `2yxh5-ojakx-r3mwz-ty6tv-k66cj-nqsw`)
- **Root folder**: Use the literal string `root` as the folder ID
- **Trash**: Use `trash` to list trashed items

## Pagination

Storage endpoints (files) use cursor-based pagination:
```bash
fastio files list --workspace WS_ID --page-size 100 --cursor NEXT_CURSOR
```

Other endpoints use offset-based pagination:
```bash
fastio org members list ORG_ID --limit 50 --offset 0
```

## Error Handling

- Exit code `0` = success
- Exit code `1` = error (check stderr for message)
- Exit code `2` = invalid arguments (clap parsing error)

Common errors:
- `authentication required` → set `--token` or run `fastio auth login`
- `workspace ID must not be empty` → missing required ID
- `invalid page size` → must be 100, 250, or 500

## MCP Server

The CLI includes a built-in MCP server for direct agent integration:
```bash
fastio mcp
```

This starts an MCP server on stdio transport, exposing all CLI operations as MCP tools.
