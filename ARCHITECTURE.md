# Fast.io CLI Architecture

## Overview

The `fastio` CLI is a Rust application providing direct access to the Fast.io REST API (`https://api.fast.io/current`). It operates in two modes:

1. **CLI mode** (default) — interactive commands for humans and scripts
2. **MCP mode** (`fastio mcp`) — Model Context Protocol server over stdio for AI agents

Both modes share a common API layer, ensuring zero code duplication.

## Layer Diagram

```
+-----------------------------------------------------------------+
|  main.rs                                                        |
|  Entry point, CLI parsing, command dispatch, MCP mode routing   |
+-----------------------------------------------------------------+
        |                    |                         |
        v                    v                         v
+----------------+   +------------------+     +------------------+
|  cli.rs        |   |  commands/       |     |  mcp/            |
|  Clap derive   |   |  25 command      |     |  MCP server      |
|  definitions   |   |  modules         |     |  22 tools        |
+----------------+   +------------------+     +------------------+
                          |         |               |
                    +-----+         +--------+      |
                    v                        v      |
            +-------------+          +------+------+
            |  api/       |          |  output/    |
            |  16 endpoint|          |  JSON,Table,|
            |  modules    |          |  CSV render |
            +-------------+          +-------------+
                    |
                    v
            +-------------+
            |  client.rs  |
            |  HTTP client|
            |  + envelope |
            |  + rate lim |
            +-------------+
                    |
                    v
            +-------------+
            |  auth/      |
            |  Token,     |
            |  PKCE,      |
            |  Credentials|
            +-------------+
                    |
                    v
            +-------------+
            |  config.rs  |
            |  ~/.fastio/ |
            +-------------+
```

## Module Responsibilities

### `main.rs`
- Tokio async entry point
- MCP mode detection — routes to MCP server before tracing init (to avoid corrupting stdio)
- CLI mode: parses args via clap, initializes tracing (stderr), loads config, dispatches to commands
- Error interception with colored output and suggestions via `CliError::render_stderr()`

### `cli.rs`
- Defines `Cli` struct with `#[derive(Parser)]`
- Global flags: `--format`, `--fields`, `--no-color`, `--quiet`, `--verbose`, `--profile`, `--token`, `--api-base`
- `Commands` enum with 25 top-level subcommands
- Nested subcommand enums for complex groups (org billing, org members, share files, task lists, etc.)

### `error.rs`
- `CliError` enum using `thiserror` with variants: `Api`, `Auth`, `Config`, `Io`, `Http`, `Parse`, `RateLimit`
- `ApiError` struct: `code`, `error_code`, `message`, `http_status`
- `suggestion()` methods providing actionable hints based on error codes and HTTP status
- `render_stderr()` for colored error display

### `config.rs`
- Manages `~/.fastio/config.json`
- `Config` struct with `default_profile` and `profiles` map
- `Profile` struct with `api_base` and `auth_method`
- Auto-creates defaults on first run
- Profile switching via `fastio configure`

### `client.rs`
- `ApiClient` struct wrapping `reqwest::Client`
- Methods: `get()`, `get_with_auth()`, `get_no_auth_with_params()`, `post()`, `post_no_auth()`, `post_json()`, `delete()`, `delete_with_form()`
- Automatic `Authorization: Bearer` header injection
- API response envelope unwrapping (`result: "yes"/"no"`)
- Rate limit header detection — warns on low remaining, returns `CliError::RateLimit` on HTTP 429
- User-Agent: `fastio-cli/<version>`
- 120-second request timeout (supports event long-polling)
- `get_token()` accessor for MCP mode token forwarding

### `auth/`

#### `credentials.rs`
- `StoredCredentials` with token, refresh_token, api_key, expires_at, user_id, email, auth_method
- `CredentialsFile` managing `~/.fastio/credentials.json`
- Per-profile credential storage with load/save/remove

#### `token.rs`
- Token resolution precedence:
  1. `--token` flag
  2. `FASTIO_TOKEN` env var
  3. `FASTIO_API_KEY` env var
  4. Profile stored credentials (API key preferred over JWT)
- Token expiration checking

#### `pkce.rs`
- RFC 7636 PKCE S256 implementation
- CSPRNG via `getrandom` crate for code_verifier and state
- Local TCP server on port 19836 for OAuth callback
- Authorization code + state extraction and CSRF validation

### `api/` — 16 Modules

Each module contains typed functions mapping to Fast.io REST endpoints:

| Module | Endpoints | Description |
|--------|-----------|-------------|
| `auth.rs` | 21 functions | Login, signup, 2FA, API keys, OAuth sessions, PKCE |
| `user.rs` | 16 functions | Profile, search, assets, invitations |
| `org.rs` | 42 functions | CRUD, billing, members, transfer, discovery, assets |
| `workspace.rs` | 38 functions | CRUD, metadata, notes, quickshares, archiving |
| `storage.rs` | 23 functions | File/folder CRUD, versions, locks, search |
| `upload.rs` | 17 functions | Sessions, chunks, finalize, web import, limits |
| `download.rs` | 3 functions | Token-based downloads, ZIP, quickshare |
| `share.rs` | 17 functions | CRUD, storage, members, password, quickshare |
| `ai.rs` | 14 functions | Chat CRUD, messages, search, summarize |
| `comment.rs` | 12 functions | CRUD, reactions, linking |
| `event.rs` | 5 functions | Search, summarize, details, polling |
| `member.rs` | 9 functions | Add, remove, transfer, leave, join |
| `workflow.rs` | 33 functions | Tasks, task lists, worklogs, approvals, todos |
| `apps.rs` | 4 functions | List, details, launch, tool-apps |
| `import.rs` | 22 functions | Providers, identities, sources, jobs, writebacks |
| `locking.rs` | 3 functions | Acquire, status, release |
| `types.rs` | — | Shared response structs |

### `commands/` — 25 Modules

Each module handles one command group, orchestrating API calls and output rendering:

| Module | Commands | Description |
|--------|----------|-------------|
| `auth.rs` | 21 | Login, 2FA, API keys, OAuth sessions |
| `user.rs` | 16 | Profile, search, assets, invitations |
| `org.rs` | 42 | Full org management with nested billing/members/invitations/transfer/assets |
| `workspace.rs` | 24 | CRUD, metadata, notes, quickshares |
| `files.rs` | 23 | Storage operations, locking, quickshares |
| `upload.rs` | 18 | Chunked upload with progress bars, session management |
| `download.rs` | 3 | Streaming download with progress bars |
| `share.rs` | 17 | Share management with nested files/members |
| `ai.rs` | 14 | Chat with async polling, message management |
| `comment.rs` | 12 | Comments, reactions |
| `event.rs` | 5 | Activity events and polling |
| `member.rs` | 9 | Member management |
| `invitation.rs` | 4 | Invitation management |
| `preview.rs` | 3 | Preview URLs and transforms |
| `asset.rs` | 3 | Asset management |
| `task.rs` | 16 | Tasks and task lists |
| `worklog.rs` | 6 | Worklog entries |
| `approval.rs` | 4 | Approval workflows |
| `todo.rs` | 7 | Todo items |
| `apps.rs` | 4 | App integration |
| `import.rs` | 22 | Cloud import/sync |
| `lock.rs` | 3 | File locking |
| `configure.rs` | 4 | CLI configuration |
| `mod.rs` | — | Module declarations |

### `mcp/` — MCP Server

#### `mod.rs`
- `FastioMcpServer` implementing rmcp `ServerHandler` trait
- Stdio transport via `rmcp::transport::stdio`
- Tool registration with `--tools` filtering
- Auth resolved at startup from credential chain
- In-session token updates via `auth` tool's `signin`/`set-api-key` actions
- Tracing disabled to keep stdout clean for JSON-RPC

#### `tools.rs`
- 22 action-routed tools with 286 total actions
- Each tool has an `action` parameter for routing (mirrors the remote MCP server pattern)
- All handlers call existing `src/api/` functions — zero duplicated API logic
- Returns MCP text content blocks with markdown-formatted data,
  byte-equivalent to the server-side `?output=markdown` contract
  (implementation in `output/markdown.rs`).
  Callers that need JSON can invoke `fastio <command> --format json` from
  the CLI side instead of the MCP path

#### `resources.rs`
- `session://status` — current auth state (authenticated, email, token expiry, scopes)

#### `prompts.rs`
- `get-started` — first-time setup guidance

### `output/`

#### `mod.rs`
- `OutputFormat` enum: Json, Table, Csv, Markdown
- `OutputConfig`: format, fields filter, no_color, quiet
- Auto-detection: table for TTY, markdown for piped output (was JSON before
  2026-04-15; markdown is roughly 3–5× more token-efficient for LLM consumers)

#### `json.rs`
- Pretty-printed JSON via `serde_json`

#### `markdown.rs`
- GitHub-flavored Markdown renderer byte-equivalent to the server-side
  `?output=markdown` contract (see
  `https://api.fast.io/current/llms/full/` for the public spec).
- Emits the `**Result:** success|failure` preamble from a scalar
  `result` field, promotes an object-valued `error` to a leading
  `# Error` section, and emits each remaining top-level key as an H1
  section in insertion order.
- Arrays of associative records render as GFM pipe tables with
  insertion-order column union (requires the
  `serde_json/preserve_order` feature); scalar lists render as
  bulleted lists; mixed lists render as bulleted lists with maps
  inlined as `**k:** v; **k:** v`.
- Value/body text is NOT escaped — the renderer takes a light-touch
  approach matching the server contract. HTML-sanitization is the
  responsibility of downstream consumers that render to HTML. Table
  cells escape only `|`, `\`, `` ` ``, and newlines; HTML-like /
  multiline cell content is wrapped in inline-code fences.
- Runtime safety rails orthogonal to the server contract: 4 MiB
  output cap, 64-frame recursion cap, 256-column table cap, and
  stripping of C0/C1 controls plus Unicode bidi / zero-width / BOM
  code points (Trojan-Source / homoglyph defense).
- The markdown path in `OutputConfig::render` does NOT go through
  `flatten_response` (unlike table/CSV) — the renderer needs the
  full envelope including `result` to produce the preamble.
- Consumers include the MCP tool-response path (markdown is the MCP
  default) and the top-level `--format markdown` CLI flag.

#### `table.rs`
- Table rendering via `comfy-table` with dynamic columns and color support

#### `csv_output.rs`
- CSV output with header row from JSON keys

#### `format.rs`
- `filter_fields()` for `--fields` support across all formats

## API Response Handling

The Fast.io API returns responses in an envelope:

```json
{
  "result": "yes" | "no",
  "response": { ... },
  "error": { "code": 1650, "error_code": 154689, "message": "..." }
}
```

The `ApiClient::handle_response()` method:
1. Checks rate-limit headers (`X-Rate-Limit-Available`, `X-Rate-Limit-Max`, `X-Rate-Limit-Expiry`)
2. Returns `CliError::RateLimit` on HTTP 429 with retry-after
3. Parses response body as JSON
4. Checks `result` field (supports `"yes"`/`"no"` strings and `true`/`false` booleans)
5. On failure: extracts `ApiError` with code, error_code, message, and HTTP status
6. On success: unwraps the `response` object and deserializes to target type

## Authentication Flows

### Basic Auth
1. User provides `--email` and `--password`
2. Base64-encode `email:password`
3. GET `/user/auth/` with `Authorization: Basic <encoded>`
4. Receive JWT token (1-hour lifetime), store in credentials
5. If `2factor: true`, prompt for 2FA verification

### PKCE Browser Flow
1. Generate code_verifier (32 random bytes via `getrandom`, base64url)
2. Derive code_challenge (SHA-256, base64url)
3. Generate random state parameter
4. GET `/oauth/authorize/` with challenge, state, client_id
5. Open browser to `https://go.fast.io/connect?auth_request_id=...`
6. Start local TCP server on `127.0.0.1:19836`
7. User authenticates in browser, callback received
8. Verify state matches (CSRF protection)
9. POST `/oauth/token/` to exchange code + verifier for tokens
10. Store access_token and refresh_token

### API Key
1. Create via `fastio auth api-key create --name "..."`
2. Store as `FASTIO_API_KEY` env var or in profile credentials
3. Used as Bearer token directly — no expiration client-side

## Error Strategy

- **Module level**: `thiserror` for structured `CliError` variants
- **Command level**: `anyhow` with `.context()` for user-friendly messages
- **API errors**: Parsed from response envelope into `ApiError` with error_code
- **Suggestions**: Context-aware hints (e.g., "Run `fastio auth login`" for 401, "Run `fastio auth verify`" for error 10587)
- **Display**: Colored output — red for errors, yellow for warnings
- **Output routing**: Errors to stderr, structured data to stdout

## Key Design Decisions

1. **Direct REST API** — calls `api.fast.io` directly, not through the MCP server, for single-hop latency
2. **Shared API layer** — both CLI and MCP modes use `src/api/`, ensuring feature parity
3. **Action-based MCP tools** — mirrors the remote MCP server's consolidated tool pattern (22 tools with action routing vs 286 individual tools)
4. **Form-encoded POST bodies** — matches the Fast.io API convention (not JSON, unless specifically required)
5. **Cursor-based pagination** — for storage endpoints; offset-based for other list endpoints
6. **CSPRNG for PKCE** — `getrandom` crate, not `HashMap::RandomState`
7. **120s HTTP timeout** — supports event long-polling (up to 95s server-side)
