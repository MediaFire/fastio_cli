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
|  Clap derive   |   |  30 command      |     |  MCP server      |
|  definitions   |   |  modules         |     |  26 action-      |
+----------------+   +------------------+     |  routed tools    |
                                              +------------------+
                          |         |               |
                    +-----+         +--------+      |
                    v                        v      |
            +-------------+          +------+------+
            |  api/       |          |  output/    |
            |  26 endpoint|          |  JSON,Table,|
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
- Error interception with colored output and suggestions via `cli_error_render()`, which renders the anyhow chain through `render_chain_dedup()` (collapsing the `#[from]` forward-to-source duplicate `thiserror` generates for `CliError::Api`) and appends the `CliError`'s own `suggestion()` as a `hint:` line

### `cli.rs`
- Defines `Cli` struct with `#[derive(Parser)]`
- Global flags: `--format`, `--fields`, `--no-color`, `--quiet`, `--verbose`, `--profile`, `--token`, `--api-base`
- `Commands` enum with 32 top-level subcommands
- Nested subcommand enums for complex groups (org billing, org members, share files, etc.)

### `error.rs`
- `CliError` enum using `thiserror` with variants: `Api`, `Auth`, `Config`, `Io`, `Http`, `Parse`, `RateLimit`, `ArtifactNotReady`, `InvalidHeaderValue` (secret can't form an HTTP header), `MappedApi` (command-layer hint override wrapper), `VersionConflict` (CAS write-back conflict)
- `ApiError` struct: `code`, `error_code`, `message`, `http_status`
- `suggestion()` methods providing actionable hints based on error codes and HTTP status (`ArtifactNotReady` returns the poll-and-retry hint instead of the generic-404 wording)
- `render_stderr()` for colored error display of a bare `CliError`; the `main.rs` interception now renders through `cli_error_render()` + `render_chain_dedup()` (chain-aware, byte-identical to `render_stderr` for a bare `CliError`)

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
- **Link-password header seam (File Shares).** `get_with_password` / `post_with_password` / `download_file_stream_with_password` attach the link password in the `x-ve-password` header (set sensitive, added to `SECRET_LOG_KEYS` so it is redacted from traces) and route through a **no-redirect** client whenever a password is present — reqwest 0.13 does NOT strip custom headers on a cross-origin 3xx, so a redirect-following client would replay the password to the `Location` target. `post_sensitive_form` is the FORM-FIELD sibling: it protects a sensitive FORM value (the password sent as a `password=…` form field on management create/update), failing closed on any 3xx so the body is never replayed — NOT a header-password path. The preview path (`download_preview_following_redirect`) follows at most ONE redirect MANUALLY, re-issuing the follow GET WITHOUT `Authorization` or `x-ve-password` (the embedded download token authorizes it) and failing closed on a second redirect

### `auth/`

#### `credentials.rs`
- `StoredCredentials` with token, refresh_token, api_key, expires_at, user_id, email, auth_method,
  scopes (the granted entity-scope list echoed by token exchange/refresh, stored verbatim as the
  server's JSON-encoded string; absent on profiles written before it existed)
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

### `api/` — 28 Modules

Count convention: **26 endpoint modules** = `src/api/*.rs` excluding `mod.rs` (declarations) and `types.rs` (shared structs; the `—` table row). The table below lists representative modules, not all 26.

Each module contains typed functions mapping to Fast.io REST endpoints:

| Module | Endpoints | Description |
|--------|-----------|-------------|
| `auth.rs` | 22 functions | Login, signup, 2FA, API keys, OAuth sessions (incl. scope narrowing), PKCE |
| `user.rs` | 16 functions | Profile, search, assets, invitations |
| `org.rs` | 42 functions | CRUD, billing, members, transfer, discovery, assets |
| `workspace.rs` | 35 functions | CRUD, metadata, notes, archiving |
| `storage.rs` | 24 functions | File/folder CRUD, versions, locks, search, extracted-text reads (`/content/` single-file windows and the workspace-only multi-file relevance read) |
| `upload.rs` | 17 functions | Sessions, chunks, finalize, web import, limits |
| `download.rs` | 2 functions | Token-based downloads, ZIP |
| `share.rs` | 16 functions | CRUD, storage, members, password |
| `search.rs` | 2 functions | Unified grouped-bucket search (`/search/`) — one query across a workspace or share, returned as `files` / `metadata` / `comments` buckets. The primary search surface; `storage.rs`'s `search_files` is the flat filename/content leg |
| `ai.rs` | 14 functions | Chat CRUD, messages, summarize (search is NOT here — see `search.rs`) |
| `comment.rs` | 12 functions | CRUD, reactions |
| `event.rs` | 5 functions | Search, summarize, details, polling |
| `member.rs` | 9 functions | Add, remove, transfer, leave, join |
| `apps.rs` | 4 functions | List, details, launch, tool-apps |
| `import.rs` | 22 functions | Providers, identities, sources, jobs, writebacks |
| `locking.rs` | 3 functions | Acquire, status, release |
| `signing.rs` | 14 functions | E-signature (SignEnvelope) — workspace-only CRUD/lifecycle, document/preview/signed/audit download paths |
| `fileshare.rs` | 10 functions | File Shares — durable single-file link shares (replacing the retired QuickShare): management create/list/update/delete + grants, password-capable anonymous consumption (details/versions), write-back path builders, websocket-auth token, named-key extractors |
| `types.rs` | — | Shared response structs |

### `commands/` — 31 Modules

Count convention: **30 command modules** = `src/bin/fastio/commands/*.rs` excluding `mod.rs` (declarations); there is no `types.rs` here. (Note: `secret_output.rs` is a shared HELPER module, not a command group — it is counted here because the convention counts every `*.rs` under `commands/` except `mod.rs`.) The table below lists representative modules, not all 30.

Each module handles one command group, orchestrating API calls and output rendering:

| Module | Commands | Description |
|--------|----------|-------------|
| `auth.rs` | 22 | Login, 2FA, API keys, OAuth sessions, scopes |
| `user.rs` | 16 | Profile, search, assets, invitations |
| `org.rs` | 42 | Full org management with nested billing/members/invitations/transfer/assets |
| `workspace.rs` | 24 | CRUD, metadata, notes |
| `files.rs` | 23 | Storage operations, locking, `content` (extracted-text chunk reads) |
| `upload.rs` | 18 | Chunked upload with progress bars, session management |
| `download.rs` | 3 | Streaming download with progress bars |
| `share.rs` | 17 | Share management with nested files/members |
| `search.rs` | 2 | Unified search across a workspace or share, with a client-side `--only` bucket filter |
| `ai.rs` | 14 | Chat with async polling, message management |
| `comment.rs` | 12 | Comments, reactions |
| `event.rs` | 5 | Activity events and polling |
| `member.rs` | 9 | Member management |
| `invitation.rs` | 4 | Invitation management |
| `preview.rs` | 3 | Preview URLs and transforms |
| `asset.rs` | 3 | Asset management |
| `apps.rs` | 4 | App integration |
| `import.rs` | 22 | Cloud import/sync |
| `lock.rs` | 3 | File locking |
| `sign.rs` | 10 | E-signature (workspace-only): envelope create/list/get/update/send/void, document download/preview/signed, audit download |
| `fileshare.rs` | 12 | File Shares: create/list/info/update/delete, grants list/add/remove, download/versions/preview, upload (write-back, CAS), activity, ws-token. `map_fileshare_error` + anonymous-capable consumption client |
| `secret_output.rs` | — | Shared helper (not a command group): `extract_secret` / `write_secret_file` (0600) / `redact_secret_field` for realtime / ws tokens (used by `fileshare`) |
| `configure.rs` | 4 | CLI configuration |
| `mod.rs` | — | Module declarations |

### `mcp/` — MCP Server

#### `mod.rs`
- `FastioMcpServer` implementing rmcp `ServerHandler` trait
- Stdio transport via `rmcp::transport::stdio`
- `--tools` allow-list: a validated set (unknown names warned to stderr and ignored; an all-unknown list is a fail-fast error) threaded into `ToolRouter` and enforced in BOTH `list_tools` (advertised set) and `call_tool` (callable set), so the two never diverge; `None` = all tools. Hidden aliases (`ai`→`ripley`, `how-to`→`howto`) are gated by their canonical name
- `list_tools` also filters out the `import` tool when the cloud-import kill-switch is off (read once at `ToolRouter` construction); the intro `instructions` enumerate only the visible tools. `import` requires BOTH the cloud-import flag AND allow-list inclusion
- Auth resolved at startup from credential chain
- In-session token updates via `auth` tool's `signin`/`set-api-key` actions
- Tracing disabled to keep stdout clean for JSON-RPC

#### `tools.rs`
- 26 action-routed tools in `TOOL_DEFS`; 25 are advertised on a default server, because `import` is filtered from `list_tools` while the cloud-import kill-switch is off. Each multiplexes many actions via its `action` parameter
- Each tool has an `action` parameter for routing (mirrors the remote MCP server pattern)
- All handlers call existing `src/api/` functions — zero duplicated API logic
- Returns MCP text content blocks with markdown-formatted data,
  byte-equivalent to the server-side `?output=markdown` contract
  (implementation in `output/markdown.rs`).
  Callers that need JSON can invoke `fastio <command> --format json` from
  the CLI side instead of the MCP path

#### `resources.rs`
- `session://status` — whether this process holds a credential (`authenticated`, `api_base`,
  and a `hint`). A purely LOCAL read: it never calls the API, and it makes no claim about
  what the credential is allowed to do — the hint points at the `auth` tool's `scopes`
  action for the live view

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
1. Checks rate-limit headers (`x-ve-limit-avail`, `x-ve-limit-max`, `x-ve-limit-expires`, falling back to the legacy `X-Rate-Limit-Available` / `-Max` / `-Expiry` for older deployments)
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
4. GET `/oauth/authorize/` with challenge, state, client_id — plus `access_mode`
   (`r`/`rwa`, from `--read-only`/`--admin`) and `account_settings=1` when requested;
   both are a CEILING the consent page may narrow
5. Open browser to `https://go.fast.io/connect?auth_request_id=...`
6. Start local TCP server on `127.0.0.1:19836`
7. User authenticates in browser, callback received
8. Verify state matches (CSRF protection)
9. POST `/oauth/token/` to exchange code + verifier for tokens
10. Store access_token, refresh_token, and the granted `scopes` echoed by the exchange

### API Key
1. Create via `fastio auth api-key create --name "..."`
2. Scope it with the structured selectors — `--org` / `--workspace` / `--share` (repeatable) and
   `--all` pick the entities, `--admin` / `--read-only` pick the access mode, `--account-settings`
   appends `userdetails:*:rw`. A shared resolver (`api::auth::resolve_key_scopes`, also used by
   `api-key update` and `oauth narrow`) turns them into the `scopes` JSON array the API expects.
   It is fail-closed: it returns nothing only when no structured flag was given, and errors on
   partial intent rather than widening. Raw `--scopes <json>` bypasses it and conflicts with it
3. Store as `FASTIO_API_KEY` env var or in profile credentials
4. Used as Bearer token directly — no expiration client-side

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
3. **Action-based MCP tools** — mirrors the remote MCP server's consolidated tool pattern (26 registered action-routed tools, 25 advertised by default since `import` is filtered, rather than one tool per individual action)
4. **Form-encoded POST bodies** — matches the Fast.io API convention (not JSON, unless specifically required)
5. **Cursor-based pagination** — for storage endpoints; offset-based for other list endpoints
6. **CSPRNG for PKCE** — `getrandom` crate, not `HashMap::RandomState`
7. **120s HTTP timeout** — supports event long-polling (up to 95s server-side)
