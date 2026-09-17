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

### Scopes and access modes

Every credential carries a list of **entity scopes**, each written
`entity_type:entity_id:access_mode` — for example `org:1234567890123456789:rw`,
`workspace:9876543210987654321:r`, or `user:*:rwa` for the whole account. The
access mode is one of:

- `r` — read only
- `rw` — read and write (the default mode once an entity is selected)
- `rwa` — read, write, and **administrative** operations on that entity

`rwa` is an access mode, not a role. Holding it does not change who the human
is, and it never grants more than the account already has — the user's own
permissions still apply on top of it.

### Requesting admin as a ceiling

`--admin` asks for the `rwa` access mode. It is a **ceiling, not a grant** — it
is the most the resulting credential may be given. What happens to a request
that reaches for more than it may have depends on where it is issued, and the
two rules are different:

- **`fastio auth login` — consent may narrow it.** The ceiling is what the
  browser session asks for; the consent page may approve less, and the
  credential ends up with whatever was approved. So the result can be smaller
  than the request.
- **`api-key create` / `api-key update` / `oauth narrow` — the issuer contains
  it.** The request is sent exactly as written, and the server **rejects** it
  when it is wider than the credential doing the issuing. It is never quietly
  trimmed: the call either fails, or returns precisely the scopes you asked for.

Either way, confirm what you actually got:

```bash
fastio auth login --admin    # request an admin ceiling, approve it in the browser
fastio auth scopes           # live view of what the credential may now do
```

`--read-only` is the same idea in the other direction: it asks for `r`. The two
are mutually exclusive, and both apply to `auth login` (browser/PKCE login
only), `auth api-key create`, `auth api-key update`, and `auth oauth narrow`.

**Selectors choose *what* the scopes cover; the access mode chooses *how much*.**
`--org`, `--workspace` and `--share` each take an entity id and may be repeated;
`--all` selects the whole account (`user:*`). Once at least one selector is
given, the access mode defaults to `rw` unless `--admin` or `--read-only` says
otherwise, and it applies to every entity in that request. **Give no selector at
all** — no `--org` / `--workspace` / `--share` / `--all`, no `--account-settings`
and no raw `--scopes` — and no scope list is sent: the credential is
**unscoped**, covering the whole account with read and write but **not**
administrative access. `rw` is the default *mode*, not what an unscoped
credential is written as.

### Account settings are a separate scope

Changing the account itself — password, email address, enrolling a second
factor (`auth 2fa setup` / `auth 2fa verify-setup`), and invalidating every
session — needs the `userdetails:*:rw` scope, requested with
`--account-settings`. **`rwa` does not grant it.** Administrative access to
organizations and account-settings access are orthogonal; ask for each one
explicitly. Signing in with 2FA (`auth 2fa send` / `auth 2fa verify`) is not
gated by it.

### Issuing and narrowing credentials

```bash
# A key that administers one org (structured flags share one access mode)
fastio auth api-key create --name "release bot" \
  --org 1234567890123456789 --admin

# Mixed access modes need the raw form: admin on one org, read on one workspace
fastio auth api-key create --name "release bot" \
  --scopes '["org:1234567890123456789:rwa","workspace:9876543210987654321:r"]'

# Give up authority on an existing login session
fastio auth oauth narrow SESSION_ID --org 1234567890123456789 --read-only
```

Points that bite agents:

- **An `api-key update` REPLACES the key's entire scope set.** There is no
  "add one scope" call. Run `fastio auth api-key get <key-id>` first, then
  re-state every scope the key should keep alongside the new one.
- **Key changes apply immediately; OAuth narrowing applies at the session's
  next token refresh.** An access token already issued keeps its original
  scopes until then.
- **`auth oauth narrow` can only give up authority**, never add it — the server
  refuses anything wider than the session already holds. To widen, sign in
  again with the ceiling you need.
- **Raw `--scopes '<json array>'` still works** for anything the structured
  flags do not express, and is mutually exclusive with them.
- **Keys issued before scopes existed** (no scope list at all) behave as
  `user:*:rw`: full read and write, **not** administrative, and **not** able to
  change account settings. Re-issue them with the access they need.
- **`full_access` does not imply admin.** It means the credential covers the
  whole account with write access rather than being narrowed to particular
  entities. The access mode is reported separately — read it there.

### Seeing what a credential may do

- `fastio auth scopes` asks the server. Authoritative and current — use this.
- `fastio auth status` shows the scopes cached locally when the profile signed
  in, so it can be stale if the grant has been narrowed since.

### When a call is refused for scope

Scope refusals are HTTP `403` carrying a machine-readable reason, and the CLI
prints a specific remedy for each. Repeating the *same* request with the same
credential never helps — something has to change: either the credential is
re-issued or re-consented with the access it lacks, or the request itself is
narrowed to something that credential can already satisfy.

- **`scope_admin_required`** — the credential covers the entity but without
  `rwa`. For an API key, read it with `api-key get`, then run `api-key update`
  re-stating its scopes plus `--admin`, from a credential that already holds
  that access. If those scopes do not all share one access mode, re-state them
  with the raw `--scopes '[...]'` form instead: `--admin` applies to every
  entity in the request, so a blanket flag would escalate a read-only scope
  along with the one you meant to raise. For a login or a web session, run
  `fastio auth login --admin` and approve admin access on the consent page.
- **`scope_exceeds_issuer`** / **`access_mode_exceeds_initiate`** — you asked
  to mint something broader than the credential doing the minting. Narrow the
  request, or issue it from a credential that already holds the access you
  asked for: a web session, or a fresh login with the ceiling you need.
- **`userdetails_scope_required`** — an account-settings operation from a
  credential without `userdetails:*:rw`. Run
  `fastio auth login --account-settings`, or add `--account-settings` when
  re-issuing the key (remembering that a key update replaces the whole scope
  set). `--admin` is not the fix here.
- **`scope_write_required`** — every access mode on the credential is `r`, so
  it is read-only and cannot perform account operations (creating an org,
  updating the user, revoking sessions, signing out). Sign in again *without*
  `--read-only`, or re-issue the key read-write (again: an update replaces the
  whole scope set, so read it back with `api-key get` first). Write is not
  admin — `--admin` asks for a higher ceiling than this needs and is not the
  fix. Sign-out is the one special case: a read-only credential cannot revoke
  its own session, so `fastio auth signout` discards the credential locally,
  reports `server_signout_completed: false`, and exits `0`.

Changing a credential's scopes is a compare-and-swap: `auth api-key update` and
`auth oauth narrow` write against the set they read, so a concurrent change
returns HTTP `409`. Do not retry the same request — re-read the credential
(`auth api-key get <key-id>` or `auth oauth details <session-id>`) and resend
the full intended set, because an update replaces the whole set and a narrow is
measured against what the session holds now.

Full endpoint reference: <https://api.fast.io/current/llms/full/>.

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
fastio id info 2abcd-efghj-kmnpq-rstuv-wxyz3-4567 NODE_ID2    # → StorageNode (hyphens OK)
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

# Keep AI indexing but stop extracting metadata from new uploads
fastio workspace update WS_ID --metadata-extraction false
```

`--metadata-extraction` is a separate opt-OUT layered under `intelligence`: it
can withhold automatic extraction from newly uploaded files, but it can never
enable extraction where the intelligence setting or the plan does not allow it.
It defaults to on, it deletes nothing, and explicit per-file extraction requests
are unaffected. It is accepted on `workspace create`, `workspace update` and
`org create-workspace`.


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

# A file's EXTRACTED TEXT (what search and AI indexed), not its raw bytes
fastio files content --workspace WS_ID NODE_ID --query "termination clause"
fastio files content --workspace WS_ID NODE_ID --chunk-from 40 --chunk-to 42
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

`fastio files search` remains the targeted file-only search. Add `--details` to
either form to attach extracted metadata facts to file and note hits in the
files bucket (workspace only; a share accepts the flag and returns no facts).

### Reading a document's text (locate and quote)

`fastio files read` returns a file's raw bytes. `fastio files content` returns
its **extracted text** as ordered chunks — the same text the platform indexed
for search and AI — so it is what actually reads a PDF's words. Each chunk
carries a `position`, which is its address.

To find a passage and quote it accurately:

```bash
# 1. Locate it. The hit carries best_chunk.position and best_chunk.indexed_version_id.
fastio files search --workspace WS_ID "termination clause" --detail standard --format json

# 2. Read that passage in context (P-1 .. P+1 around best_chunk.position P).
fastio files content --workspace WS_ID NODE_ID --chunk-from 39 --chunk-to 41 --format json

# 3. Confirm the response's indexed_version_id MATCHES the search hit's before
#    quoting. If it differs the file was re-indexed — re-run the search.
#    A hit whose best_chunk.position is null has no address — use --query
#    on that file instead of a chunk range.
```

When `best_chunk.same_as_snippet` is true its `text` is null at
`--detail terse`/`standard`; read `content_snippet` on the hit instead.
`--detail terse` on `files content` returns the chunk map with no text.

```bash
# Score up to 10 candidate files against one question in a single request
# (workspace only; scores are comparable only within one file, never across files).
fastio files content --workspace WS_ID --nodes NODE_A,NODE_B,NODE_C --query "renewal terms"

# Walk a whole file: pass the previous response's next_cursor back verbatim,
# and stop when it is null.
fastio files content --workspace WS_ID NODE_ID --cursor "NEXT_CURSOR"
```

`indexed: false` is a normal answer, not a failure — that version has no
extracted text yet, or carries none at all.

## E-signature

`fastio sign` and the `sign` MCP tool drive **`SignEnvelopes`** — PDFs sent to
recipients for electronic signature. Every envelope is parented to a
**workspace**, so each subcommand takes a required `--workspace <id>`.

Signing is available on **every plan**. The org resource exposes
`capabilities.signing` (boolean) to confirm availability, and the server rejects
calls with a feature-disabled error if an org's plan does not grant it — a `403`
with error code `1670` ("Signing is not enabled for this organization"). Access
also requires workspace membership.

Over MCP the `sign` tool exposes reads, reversible draft-drive actions, and the
idempotent recovery actions. The outward-facing and terminal actions — `send`
(which emails real recipients) and `void` — are **CLI-binary-only**
(`fastio sign envelope send|void …`), as are `template create|update|delete`.
Envelopes are voided, never deleted.

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
  (e.g. `1234567890123456789`)
- **Node IDs** (files/folders): opaque alphanumeric with hyphens
  (e.g. `2abcd-efghj-kmnpq-rstuv-wxyz3-4567`)
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
(`ripley`, `fileshare`, `files`, `org`, `workspace`, `sign`, …). The `files`
tool mirrors the three ways to reach a file's words: `read` for raw bytes,
`content` for one file's extracted text as addressed chunks, and `content-many`
to score up to 10 files against one question in a single call. Tool results are
rendered as GitHub-flavored Markdown for compact, high-signal consumption. This
same guide is available as the `skill://guide` MCP resource and via
`fastio skill`.

Auth and backend follow the standard CLI precedence: the server honors the
global `--token` / `--profile` / `--api-base` flags (and `FASTIO_TOKEN` /
`FASTIO_API_KEY` env), so `fastio --profile staging mcp` or
`fastio --api-base <url> mcp` point it at a non-default backend.
