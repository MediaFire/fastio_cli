# Fast.io CLI — Agent Guide

Use this guide when interacting with the Fast.io platform via the `fastio` CLI
(and its built-in MCP server, `fastio mcp`).

## Offload to Ripley first

Fast.io ships a delegated subagent — **Ripley**, Fast.io's AI agent. Before
firing a long chain of low-level primitives, consider asking Ripley to find or
do the multi-step work for you. Ripley acts **on the user's behalf** (it carries
the caller's JWT and runs in a sandbox), so it can search, read, summarize, and
chain operations across a workspace or share that would otherwise take many
round-trips.

```bash
# Ask Ripley a question and get the answer (creates a chat, waits for the result)
fastio ripley ask --workspace WS_ID "Which contracts mention auto-renewal?"

# Same, scoped to a share
fastio ripley ask --share SHARE_ID "Summarize the latest revision of the proposal"
```

Heuristics for agents:

- **Prefer one `ripley ask` over many primitives** when the task is "find/answer/
  summarize across content." Reserve the raw `files`/`storage`/`search` commands
  for deterministic, single-shot operations where you already know the IDs.
- **Poll activity, not detail.** For anything that runs asynchronously (a Ripley
  answer, a metadata extraction job), watch the activity/state endpoint with a
  bounded wait — do **not** tight-loop a `--detail full` read. The `ask` and
  `metadata extract --wait` paths already do bounded activity-polling for you.

`ripley` is the former `ai` group — **`ai` still works as a hidden alias** (CLI
and MCP) for backward compatibility, but new code should use `ripley`.

> **Deferred / pending.** The Ripley delegated-**job** lifecycle
> (`ripley delegate` / `run` / `status` / `logs` / `cancel-job`) is **not yet
> available** — it is pending the server-side delegation contract, which has not
> been finalized. Those verbs are hidden stubs that call no endpoint and exit
> with a "not yet available" message. Until the contract ships, delegate work via
> `ripley ask` / `ripley chat`, which run today.

## Authentication

Authenticate before using any command. Two methods:

```bash
# Option 1: API key (best for agents/automation)
fastio --token YOUR_API_KEY auth check

# Option 2: PKCE browser login (interactive)
fastio auth login
# Opens a URL → sign in → paste the authorization code
```

For automation, pass `--token` on every command or set `FASTIO_TOKEN`:

```bash
export FASTIO_TOKEN=your_api_key
fastio org list
```

## Output Format and verbosity

Two orthogonal knobs control output:

- `--format` controls **client-side rendering**: `table` (human), `json`
  (programmatic), `csv` (spreadsheets), `markdown`/`md` (GitHub-flavored).
  Default is `table` for a TTY and `markdown` for pipes (changed from `json` on
  2026-04-15 to benefit LLM consumers; pass `--format json` for the old shape).
- `--detail terse|standard|full` controls **server-side verbosity** by passing
  `?output=<detail>` on supported endpoints — i.e. how much data the API returns.
  It is independent of `--format`. Omitting it uses the server's `full` shape.

```bash
fastio org list --format json
fastio files list --workspace WS_ID --detail terse --format json
```

Use `--quiet` to suppress output (useful for write operations where you only
care about the exit code).

### Viewing markdown

`fastio view <workspace_id> <node_id>` renders a markdown note (or a `.md`
file) in the terminal. It always emits rendered markdown — or verbatim with
`--raw`, when piped, or with `--no-color` — and ignores `--format`/`--fields`.
Only note nodes and markdown files are supported; other file types are rejected.

### Inspecting identifiers (offline)

`fastio id info <ID>...` classifies one or more Fast.io `OpaqueId`s **offline**
(no auth, no network) so you know what an id refers to before acting on it —
handy when an id arrives in a webhook, event, or payload. It reads the
self-describing length and type prefix: 1-char-type ids are 29 chars; 2-char-type
ids are 30 chars (this includes the legacy workflow family under `w`, retired
2026-07 but still decodable for ids minted before the sunset). It reports the
`entity_type`, `family`, `surfacing` tier, and a `recognized` flag. Output is
always an array of records, so `--format json|table|csv|markdown` and `--fields`
all apply.

```bash
fastio id info 2yxh5-ojakx-r3mwz-ty6tv-k66cj-nqsw NODE_ID2    # → StorageNode (hyphens OK)
```

Over MCP this is the `id` tool (`action: "info"`, params `id` or `ids`). A
29-char id whose 1-char code is unmapped is reported `unknown` (it may be a
transitional code pending reassignment), never guessed.

## Important: Intelligence (AI indexing) Setting

Workspaces have an `intelligence` toggle. When **OFF** (default) the workspace is
pure storage. When **ON**, documents are indexed with embeddings for AI-powered
search, chat, and summarization. Ingestion is expensive (per-page cost), so only
enable it on workspaces you intend to query.

```bash
# Storage-only (default, recommended)
fastio workspace create --org ORG_ID --name "File Storage"

# AI/RAG use case
fastio workspace create --org ORG_ID --name "Knowledge Base" --intelligence true

# Toggle AI indexing on an existing workspace
fastio workspace update WS_ID --intelligence true
```


## Core Workflows

### Organizations, workspaces, files

```bash
fastio org list --format json
fastio workspace list --org ORG_ID --format json

fastio files list --workspace WS_ID --folder NODE_ID --format json
fastio files search --workspace WS_ID --limit 25 "query" --format json
fastio files create-folder --workspace WS_ID --parent NODE_ID --name "My Folder"
fastio files delete --workspace WS_ID NODE_ID        # → trash
fastio files purge  --workspace WS_ID NODE_ID        # permanent
```

### Upload / download

```bash
fastio upload file --workspace WS_ID --folder FOLDER_NODE_ID ./path/to/file
fastio upload text --workspace WS_ID --name "notes.txt" "File content here"
fastio upload url  --workspace WS_ID --url "https://example.com/file.pdf"

fastio download file   --workspace WS_ID --node-id NODE_ID
fastio download folder --workspace WS_ID --node-id FOLDER_NODE_ID   # ZIP
```

### Shares (data rooms)

```bash
fastio share list --format json
fastio share create "Share Name" --workspace WS_ID
fastio share info SHARE_ID --format json
fastio share guest-auth SHARE_ID
```

### Ripley (AI agent)

Requires `intelligence` enabled on the workspace for content-aware queries.

```bash
fastio ripley ask --workspace WS_ID "your question"      # headline verb
fastio ripley list --workspace WS_ID --kind all          # chats
fastio ripley details CHAT_ID --workspace WS_ID
fastio ripley messages CHAT_ID --workspace WS_ID
fastio ripley summary --workspace WS_ID NODE_ID1 NODE_ID2     # AI share
```

### Unified search

`fastio search` runs one query across grouped result buckets (files, metadata,
comments) for a workspace or share:

```bash
fastio search workspace WS_ID "query" --format json
fastio search share SHARE_ID "query" --only files,comments
```

`fastio files search` remains the targeted file-only search.

## E-signature (disabled by default)

The e-signature surface (`fastio sign` and the `sign` MCP tool) is **disabled by
default** as of the 2026-07 feature sunset. An operator re-enables it by setting
`FASTIO_ENABLE_ESIGN=1`; signing must **also** be enabled for the organization
server-side. While disabled, the `sign` subcommand is hidden from help and its
execution is gated, and the `sign` MCP tool is filtered from the tool list.

## File Shares

`fastio fileshare` creates **durable, link-shareable views of a SINGLE workspace
file** — the replacement for the retired **QuickShare** (the legacy QuickShare
surface has been fully removed from this CLI). A File Share binds one file node
and serves it via a stable link with an optional password, an access option, an
expiry, and per-user grants (`view < download < edit`).

```bash
fastio fileshare create --workspace WS_ID --node NODE_ID --title "Q3 Report" --access-option anyone_with_link
fastio fileshare list   --workspace WS_ID
fastio fileshare info   FS_ID                       # details + effective_capability (anonymous-capable)
fastio fileshare update FS_ID --title "..." --clear-password --clear-expires
fastio fileshare grants list  FS_ID
fastio fileshare grants add   FS_ID --user USER_ID --capability download   # exactly one of --user / --email
fastio fileshare grants remove FS_ID --email someone@example.com --yes
fastio fileshare download FS_ID -o ./file.pdf [--version VID] [--password PW]
fastio fileshare versions FS_ID
fastio fileshare preview  FS_ID --type thumbnail -o ./thumb.jpg            # PRIMARY asset only
fastio fileshare upload   FS_ID ./new-version.pdf [--if-version VID] --yes  # write-back: NEW VERSION
fastio fileshare activity FS_ID                     # single activity poll (members only)
fastio fileshare ws-token FS_ID --token-file ./ws.token                    # realtime token (0600)
fastio fileshare delete   FS_ID --yes
```

- **Anonymous consumption.** `info` / `download` / `versions` / `preview` may be
  used **without auth** on a public (`anyone_with_link`) share. A protected link
  needs the password (next bullet). An **expired stored-profile** credential
  falls back to anonymous for these reads (with one stderr warning); an explicit
  `--token`/env token that fails stays a hard error; management / `upload` /
  `ws-token` / `activity` always require auth.
- **Password discipline.** A link password comes from `--password` **or** the
  `FASTIO_FILESHARE_PASSWORD` env var (the flag wins; prefer the env var so the
  value stays out of `ps` and shell history). It travels **only** in the
  `x-ve-password` header and is never logged. On `update`, pass
  `--clear-password` to remove it (don't also pass `--password`). A `1650`/`401`
  on a consumption read means the link password is missing or wrong (not an
  account-login problem).
- **Write-back (CAS).** `fileshare upload` pushes a **new version** of the bound
  file (the previous version is retained in history) and needs an `edit` grant.
  Pass `--if-version VID` for optimistic concurrency: the precondition is
  **server-enforced** — when the server detects a version conflict it reports
  `CONFLICT_VERSION_MISMATCH:{vid}` and the CLI surfaces it as a version-conflict
  error. On that conflict, re-download the current bytes, re-apply your change,
  and retry with `--if-version {vid}`. Files ≤ 4 MB go single-shot; larger files
  chunk + complete + poll.
- **`ws-token`** mints a realtime WebSocket token; it is **redacted from stdout**
  and only written (0600) to `--token-file <path>`. There is no in-CLI WebSocket
  client (token mint only).

### Over MCP

The `fileshare` MCP tool exposes **read + drive** actions: `create`, `list`,
`info`, `update`, `grants-list`, `grants-add`, `versions`, `download`, `preview`,
`activity`, `describe`. The four LINK-ACCESS reads (`info`, `download`,
`versions`, `preview`) run **anonymously** when the server holds no token — the
same anonymous-consumption path as the CLI (a `named_people` / `any_registered`
share then returns the uniform unavailable/auth error). The other actions
require auth. The destructive actions are **confirm-gated**: `delete` requires
`confirm_delete=true` and `grants-remove` requires `confirm_revoke=true`
(rejected **before auth and arg extraction**, so even an unauthenticated /
arg-less probe gets the gate message, mirroring the CLI `--yes`). The
`password` arg must be a **string**; a non-string value (or `password` together
with `clear_password=true` on `update`) is rejected explicitly. Two actions are
**CLI-binary-only** and NOT routable over MCP:

- **`upload`** (write-back) — it needs the local file bytes and is destructive;
  run `fastio fileshare upload …`.
- **`ws-token`** (realtime mint) — the token is a long-lived secret that must not
  be parked in an MCP transcript (the CLI redacts it and writes 0600); run
  `fastio fileshare ws-token … --token-file <path>`.

`download` / `preview` write bytes to the agent's local filesystem (default under
`.fastio/downloads/`, created 0700) and return a path + byte count. The
`password` arg authorizes a protected link (x-ve-password; never echoed).

## Billing

Org billing lives under `fastio org billing`:

```bash
fastio org billing plans
fastio org billing subscribe ORG_ID --plan PLAN_ID
fastio org billing reactivate ORG_ID         # re-enable a scheduled cancel
fastio org billing cancel ORG_ID --yes       # schedule cancel at period end
fastio org billing usage ORG_ID              # credit usage
fastio org billing meters ORG_ID --meter METER [--workspace-id ID | --share-id ID]
fastio org billing invoices ORG_ID [--starting-after CURSOR]
fastio org billing details ORG_ID
fastio org billing members ORG_ID
```

A `402` / billing error surfaces an actionable hint pointing at
`fastio org billing plans` / `subscribe`.

## ID Formats

- **Organization / Workspace / Share IDs**: 19-digit numeric strings
  (e.g. `3867689418901071163`)
- **Node IDs** (files/folders): opaque alphanumeric with hyphens
  (e.g. `2yxh5-ojakx-r3mwz-ty6tv-k66cj-nqsw`)
- **Root folder**: the literal string `root`
- **Trash**: the literal string `trash`

## Pagination

Storage endpoints (files) use cursor-based pagination:

```bash
fastio files list --workspace WS_ID --page-size 100 --cursor NEXT_CURSOR
```

Other endpoints use offset-based pagination (and a few — e.g. invoices — use
cursor / `--starting-after`):

```bash
fastio org members list ORG_ID --limit 50 --offset 0
```

## Error Handling

- Exit code `0` = success
- Exit code `1` = error (check stderr)
- Exit code `2` = invalid arguments (clap parsing error)

Common errors:

- `authentication required` → set `--token` or run `fastio auth login`
- `workspace ID must not be empty` → missing required ID
- `invalid page size` → must be 100, 250, or 500
- a `402` billing error → run `fastio org billing plans` / `subscribe`

## MCP Server

The CLI includes a built-in MCP server for direct agent integration:

```bash
fastio mcp
```

It speaks MCP over stdio and exposes the CLI's operations as action-routed tools
(`ripley`, `fileshare`, `files`, `org`, `workspace`, … plus `sign` only when
E-Sign is enabled). Tool results are
rendered as GitHub-flavored Markdown for compact, high-signal consumption. This
same guide is available as the `skill://guide` MCP resource and via
`fastio skill`.

Auth and backend follow the standard CLI precedence: the server honors the
global `--token` / `--profile` / `--api-base` flags (and `FASTIO_TOKEN` /
`FASTIO_API_KEY` env), so `fastio --profile staging mcp` or
`fastio --api-base <url> mcp` point it at a non-default backend.
