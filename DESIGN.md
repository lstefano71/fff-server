# fff-server — design

A REST server over [fff](https://github.com/dmtrKovalenko/fff), exposing its file-search
engine as a typed HTTP API.

## Why this exists

fff already ships an MCP server, but MCP returns **prose**. The MCP layer formats results
into text for an LLM and discards real data in the process: match highlight ranges
(`match_byte_offsets`), `col`, `byte_offset`, `fuzzy_score`, `location`,
`total_files_searched`, `filtered_file_count`, `regex_fallback_error`, and everything in the
score breakdown beyond `total`. It also hardcodes roughly thirteen grep options, caps output
at 2.5–5 KB, and truncates lines at 180 characters.

This server returns typed objects specified as precisely as the internal Rust API allows,
for programmatic consumers.

## Fundamental choices

**Rust + axum against the `fff-search` crate directly.** Every other binding (C ABI →
`bun:ffi`/`ffi-rs` → TypeScript, PyO3 → Python) re-states the type definitions by hand, and
that restatement has already drifted: the Rust `Score` in 0.11 has **13** fields, the C
`FffScore` drops `git_status_boost`, and the TypeScript `Score` carries only 10 — missing
`git_status_boost`, `path_alignment_bonus`, and the `git_recency_boost` that 0.11 added.
Binding in Rust means the OpenAPI document is generated from the real structs, so the
published contract cannot drift from the engine.

It also buys two things no binding exposes: `GrepSearchOptions::abort_signal`, for genuine
cancellation of an in-flight grep; and real concurrency, since fff's own rayon pools do the
work instead of a single blocked JavaScript event loop.

**Multi-root.** The MCP server is single-root and stdio-only — the indexed root is fixed at
startup, so multi-repo means one process per repo. This server holds a pool of workspaces,
one `SharedFilePicker` each, which is the shape `fff-search` was built for
(`SharedFilePicker` is `Clone + Send + Sync` and is already cloned across threads throughout
the engine).

## Consumer

Another program over HTTP, expected to be .NET. Several conventions follow from that and are
noted as such below — where a choice trades byte-parity with the other bindings for a client
that needs no post-processing, the client wins. That is the point of the project.

## Endpoints

### Workspaces

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/workspaces` | Create or warm. Canonicalises and dedupes, so any spelling of a root maps to one id |
| `GET` | `/v1/workspaces` | List |
| `GET` | `/v1/workspaces/{id}` | Status: scan progress, git root, `isNetworkPath`, `lastScanAt`, config echo |
| `DELETE` | `/v1/workspaces/{id}` | Evict now, releasing memory and LMDB handles |

`POST /v1/workspaces` blocks until the initial scan completes, up to
`default_wait_for_index_ms` (30 s), then returns with the workspace marked still-indexing
rather than failing. It doubles as the warm-up call: a client that knows its roots can index
them at startup and never hit the cold path.

Readiness has **three** stages, and they are not simultaneous (measured, see Verified
below): `wait_for_scan` means files are searchable; `wait_for_indexing_complete` means the
content index is built, which fuzzy grep needs and which the watcher is gated behind;
`wait_for_watcher` means live updates are flowing. Creation reports all three as
`isScanning` / `isWarmupComplete` / `isWatcherReady`, with an optional
`waitFor: scan | indexing | watcher`. Waiting only for the scan and then immediately
fuzzy-grepping would search an incomplete content index.

**Creation never blocks for long.** An 87k-file share took 59.5 s to scan, and holding an
HTTP request open that long invites client timeouts. So creation blocks for at most
`create_block_ms` (default 10 s) and then returns **`202 Accepted`** with the workspace
resource and a status URL; a client polls `GET /v1/workspaces/{id}`. Local roots complete
inside the window and return `201` directly, so the simple case stays a single call.

Creation body: `root` (required), `aiMode`, `contentIndexing`, `watch`, `followSymlinks`,
`cacheBudget`, `gitRecency`, `waitForIndexMs`.

### Search

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/workspaces/{id}/search` | Files |
| `POST` | `/v1/workspaces/{id}/search/directories` | Directories |
| `POST` | `/v1/workspaces/{id}/search/mixed` | Files and directories |
| `POST` | `/v1/workspaces/{id}/glob` | Literal glob, bypassing the query parser |

### Grep

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/workspaces/{id}/grep` | `mode`: `plain` \| `regex` \| `fuzzy` \| `auto` |
| `POST` | `/v1/workspaces/{id}/multi-grep` | `patterns[]` OR-matched via Aho-Corasick |

`mode: "auto"` selects between plain and regex using `has_regex_metacharacters`, as the MCP
server does.

### Lifecycle and tracking

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/workspaces/{id}/rescan` | Force a rescan |
| `POST` | `/v1/workspaces/{id}/git/refresh` | Refresh git status, returns count updated |
| `POST` | `/v1/workspaces/{id}/track-access` | Report a file was opened, feeding frecency |
| `POST` | `/v1/workspaces/{id}/track-query` | Report which result a query led to, feeding combo-boost |
| `GET` | `/v1/workspaces/{id}/history?offset=N` | Historical query |

`reindex(newPath)` is deliberately **not** exposed: changing a workspace's root would break
the identity guarantee below. `DELETE` then `POST` instead.

### Server

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/parse-query` | Decompose a DSL string into constraints, fuzzy terms, location, warnings |
| `GET` | `/v1/health` | Server and per-workspace health |
| `GET` | `/openapi.json` | The contract |

## The query DSL

fff's power is its query language: `git:modified src/**/*.rs !src/**/mod.rs user controller`
parses into a constraint vector, a fuzzy term, and an optional `file.ts:42:10` location.

The raw string is the primary interface — it is what the parser consumes, and it is what
every other binding passes through. Reimplementing it as JSON would mean reimplementing
`ParserConfig`'s eleven toggles and five presets in the contract.

Structured constraints are accepted as an alternative, so a client building a query
programmatically need not concatenate and escape strings.

`POST /v1/parse-query` shows a client how a string decomposed, and warns when a glob-looking
token contains a backslash (see Separators).

## Data shapes

```
FileItem
  relativePath   string     forward slashes, on-disk casing
  fileName       string
  directory      string     trailing '/', "" at root
  absolutePath   string     native backslashes
  size           int64
  modified       date-time  RFC 3339 UTC
  isBinary       bool
  accessFrecencyScore        int
  modificationFrecencyScore  int
  gitRecencyScore            int
  totalFrecencyScore         int    access + modification only, NOT git recency
  gitStatus      { status: string|null, flags: string[] }

Score
  total, baseScore, filenameBonus, specialFilenameBonus, frecencyBoost,
  gitStatusBoost, gitRecencyBoost, distancePenalty, currentFilePenalty,
  comboMatchBoost, pathAlignmentBonus, exactMatch, matchType

SearchResponse
  items[{ item, score, matchRangesUtf16 }]
  totalMatched, totalFiles, location?, page, pageSize, hasMore

GrepResponse
  matches[{ fileIndex, lineNumber, colUtf16, byteOffset, lineContent,
            matchRangesUtf16, fuzzyScore?, isDefinition?,
            contextBefore[], contextAfter[] }]
  files[FileItem]
  totalMatched, totalFilesSearched, totalFiles, filteredFileCount,
  filesWithMatches, nextCursor?, regexFallbackError?, literalFallback
```

`Score` carries all 13 fields, including the `gitStatusBoost`, `pathAlignmentBonus`, and
`gitRecencyBoost` that the C ABI and every existing binding lose.

`FileItem.gitRecencyScore` is exposed separately because `total_frecency_score()` sums only
the access and modification halves — git recency would otherwise be invisible to a client
even though it contributes to ranking.

### Deliberate deviations from the Rust shapes

**Zipped hits.** fff returns `items`, `scores`, and `match_byte_offsets` as three parallel
arrays. These are zipped into one object per hit. Nothing is lost and generated C# is far
better for it.

**Grep keeps `files[]` + `fileIndex`.** Faithful to fff, and avoids repeating a file's full
metadata across hundreds of matches in the same file.

**UTF-16 offsets.** `match_byte_offsets` and `col` are UTF-8 **byte** offsets in Rust. .NET
strings are UTF-16, so on any line containing non-ASCII those offsets silently address the
wrong characters. They are converted server-side and named `matchRangesUtf16` / `colUtf16`
so the units are unambiguous. `byteOffset` (an absolute file offset, used for seeking) stays
a byte count, as it must.

**RFC 3339 timestamps.** `FileItem.modified` is a `u64` of Unix seconds in Rust; it is
emitted as an RFC 3339 string so it binds to `DateTimeOffset` with no client code.

**Git status as flags.** `git2::Status` is a bitflags value — a file can be simultaneously
staged-modified and worktree-modified. Every existing binding flattens it to a single string
and loses that. Both are emitted: a `status` convenience string and a complete `flags` array.

### Case sensitivity on grep

0.11 replaced the `smart_case` boolean with `casing: Option<Casing>`, where `Casing` is
`Smart` (case-insensitive unless the pattern contains an uppercase character), `Sensitive`,
or `Insensitive`. The boolean remains only as a legacy fallback, ignored when `casing` is set.

The request exposes `casing` as a **closed** enum — unlike `matchType`, this is a real Rust
enum, so enumerating it in the contract is safe and gives the C# client a proper enum. The
legacy boolean is not offered.

### Conventions

camelCase, matching the TypeScript SDK and ASP.NET Core's default naming policy.
`matchType` is documented as an **open** string (`fuzzy`, `exact`, `prefix`, `path`, …)
because it is a bare `&'static str` in Rust with no enum backing it; modelling it as a closed
enum would turn an upstream addition into a client-side deserialisation failure.

## Paths

The index is `/`-canonical on every platform — relative paths are folded to forward slashes
at scan time via `to_canonical_slashes`. `base_path`, by contrast, is dunce-canonicalised on
Windows: uppercase drive letter, on-disk casing, backslashes, symlinks resolved, no `\\?\`
prefix (deliberately, so it stays byte-comparable with libgit2's `workdir()`).

So:

- `relativePath` is forward-slash form, as fff stores it. It is what a client matches,
  globs, and displays.
- `absolutePath` is fully native, built via **`write_absolute_path`**, never
  `FileItem::absolute_path` — the latter returns mixed separators on Windows (confirmed
  live: `D:\devel\fff-server\spike/unc-probe/Cargo.toml`), which Win32 accepts for I/O but
  which is byte-equal to nothing the OS or git will hand back.
- `absolutePath` is also **stripped of any verbatim prefix** for presentation:
  `\\?\UNC\server\share\x` is emitted as `\\server\share\x`, and `\\?\D:\x` as `D:\x`.
  Share-backed workspaces always have a verbatim base path (see Workspace identity), and
  `\\?\`-prefixed strings break plenty of .NET and UI code that does not expect them. The
  contract documents `absolutePath` as display-and-open-safe, **not** an identity key —
  identity is the file ID, and the unprefixed form is what a client can actually use.

Root paths are length-validated at workspace creation, because `write_absolute_path` writes
into a fixed 4096-byte buffer on Windows and panics past it.

### Workspace identity

Roots must be deduplicated: two workspaces over one tree mean double the memory and, on a
large share, two multi-minute scans.

**Path strings cannot provide that identity on Windows shares.** Measured: the same
directory, reached two ways, canonicalises to two different strings.

```
via UNC   \\?\UNC\azwesofia10\diskk\users\Stefano\Sofia\s81alfa01
via K:\   \\?\UNC\AZWEsofia10.scdom.net\diskK\users\Stefano\Sofia\s81alfa01
```

Short hostname versus FQDN, and `diskk` versus `diskK`. Case-folding reconciles the share
name but not the hostname — those are two names for one host, and resolving between them
reliably is not attempted.

**So identity is the directory's filesystem identity, not its path**: volume serial + file ID
(`FILE_ID_INFO`, as the `file-id` crate reads it — already a transitive dependency of
`fff-search`). Two spellings of one directory yield the same pair. Where the filesystem
supplies no usable ID, the key falls back to the case-folded canonical path string, which
degrades to string identity rather than failing.

Two measured facts simplify this:

- **Mapped drives resolve to UNC automatically.** `k:\users\stefano\...` canonicalises to
  `\\?\UNC\...` unaided, so no drive-mapping code is needed. This also disposes of the
  service-account hazard: a drive mapping is per-logon-session, but the resolved UNC path is
  not.
- **Canonicalisation fixes casing**, taking a typed `stefano` to the on-disk `Stefano`.

**On shares, `base_path` keeps the verbatim `\\?\UNC\` prefix.** `dunce::canonicalize` does
*not* simplify UNC paths — measured, and contrary to what an earlier draft of this document
asserted. Every share-backed workspace therefore has a verbatim base path, unavoidably. That
is harmless for identity and for I/O (grep read files successfully through it); see Known
risks for the git consequence, and Paths above for why a client never sees it.

Note a bare share root (`\\server\share`) is rejected by fff as a filesystem root — its
`parent()` is `None`. `\\server\share\project` is fine.

Canonicalisation is performed by this server and fails the request loudly on error, rather
than relying on `FilePicker::new`'s `canonicalize(...).unwrap_or(path)`, which silently
degrades every downstream invariant.

### Separators in queries

The DSL is `/`-only where it counts: glob patterns reach `globset`, which treats **only**
`/` as a separator, so `src/**/*.rs` works and `src\**\*.rs` silently matches nothing.
Path-segment tokens likewise need `/` for the parser to classify them.

Folding `\` to `/` across the whole query would be wrong — a backslash is legitimate content
in a grep pattern (regex escapes, Windows paths in source, `\r\n`). So folding is applied
**only** in the structured constraint fields (`glob`, `pathSegments`, `exclude`), where a
separator is unambiguously a separator. Raw queries and grep patterns pass through untouched,
and `parse-query` warns about backslashes in glob-looking tokens so the failure is
discoverable rather than silent.

## Concurrency and cancellation

Searches are synchronous CPU-bound Rust that fan out internally across fff's
`SEARCH_THREAD_POOL`, so they must not run on tokio's async workers. Each request runs under
`spawn_blocking` with a lowered blocking-pool cap.

Both grep routes wire `GrepSearchOptions::abort_signal` to client disconnect, so a cancelled
`HttpClient` request actually stops the work. Note the asymmetry: **grep has `abort_signal`,
fuzzy search does not**, so a search request runs to completion regardless. A default
`time_budget_ms` bounds grep as the only other lever.

## Index posture

Per workspace, overridable at creation:

- **Content indexing: on.** Builds the bigram index, roughly 360 bytes per indexed file
  (~36 MB per 100k files). It is what makes typo-resistant and fuzzy grep work at all.
- **Watcher: on.** On Windows this is a single recursive watch per workspace.
- **Cache budget: auto.** `ContentCacheBudget::new_for_repo` tiers by file count: >50k files
  → 5k files/128 MB; >10k → 10k/256 MB; otherwise 30k/512 MB.
- **`FFFMode::Ai`** by default — it changes frecency decay constants, modification-score
  thresholds, and watcher event handling, and the consumer is a program, not a human
  scrolling a picker.
- **Git recency: on**, at fff's defaults (`enabled`, `max_commits: 10`,
  `max_files_per_commit: 50`, hard-capped internally at 128 commits). New in 0.11: it boosts
  files touched by recent commits on the current branch, and it is the one ranking signal that
  works well for this server without any client cooperation — unlike the access half of
  frecency, it needs no `track-access` calls. `maxCommits` and `maxFilesPerCommit` are
  overridable per workspace; `max_files_per_commit` exists to discard sweeping commits that
  touched the whole tree.

`enable_mmap_cache` is **not** offered: on Windows the mmap content cache is compiled out
entirely — `get_cached_content` returns `None` unconditionally, `invalidate_mmap` is a no-op,
and `MmapSlot` is `()`.

### Freshness: watcher plus adaptive rescan

The watcher is **on for every workspace**, network roots included. An earlier draft disabled
it on shares, on the theory that `ReadDirectoryChangesW` is unreliable over SMB. That was
wrong on both counts: measured, a new file on a UNC share was indexed in **~500 ms**,
identical to local disk. The one run where the watcher never appeared was an 87k-file tree
whose content indexing had not finished — the watcher is installed *after* the post-scan
phase, so a slow content index defers it. That is a sequencing consequence, not an SMB fault.

A periodic rescan still backs it up, because a silently stale index is the one failure mode
that would make this server worse than spawning ripgrep. But a fixed interval is wrong at
both ends of the observed range: rescanning an 87k share costs over a minute, so a 300 s
interval would spend a fifth of the server's life rescanning, while a 159-file slice rescans
in seconds and can afford to be brisk.

So the interval is **derived from measured cost**:

```
interval = clamp(time_to_ready × duty_factor, min_interval, max_interval)

duty_factor  = 25       # keep rescan cost near 4% of wall-clock
min_interval = 60 s
max_interval = 1800 s
```

`time_to_ready` is time to *indexing-complete*, not to scan-complete. This matters more than
it looks: a 159-file slice scanned in 374 ms but reached indexing-complete at 7.76 s —
content indexing was 20× the walk. A rescan pays both phases, so keying off scan duration
alone would underestimate its cost twentyfold.

Keying off measured cost rather than `isNetworkPath` is deliberate. What varies is how
expensive a rescan is, which is a property of tree size and per-file overhead, not of
transport — and on these machines the dominant cost is client-side antivirus (`MsMpEng`),
which no path-type heuristic would predict.

`POST /rescan` is always available and is the precise tool: a client that knows it changed
something should say so rather than wait for a timer. `isNetworkPath`, `lastScanAt`,
`lastScanDurationMs`, and the effective `rescanIntervalSecs` are all exposed on the workspace
resource, so the behaviour is never a mystery.

### Eviction is cost-aware too

Idle eviction reclaims a workspace's memory and LMDB handles, but dropping an index means the
next caller pays for rebuilding it — and on the 87k share that is over a minute of
mostly-antivirus time. A flat hour would discard an expensive index after one quiet hour and
charge the next request for it.

So the idle timeout scales by the same measured cost:

```
idle = clamp(time_to_ready × idle_factor, idle_min, idle_max)

idle_factor = 120
idle_min    = 1800 s      # 30 min
idle_max    = 86400 s     # 24 h
```

Cheap workspaces expire in half an hour; the 87k share survives roughly two hours of silence.
The cost of keeping an index is memory, the cost of dropping it is a rescan, and pricing that
trade off measured cost needs no client involvement. Setting `idle_min` equal to `idle_max`
flattens it to a fixed timeout; `DELETE /v1/workspaces/{id}` evicts on demand regardless.

## Frecency and query history

Both LMDB databases are enabled, with per-workspace paths (the LMDB env pool actively
rejects sharing with `EnvSpecMismatch`/`DbInUse`).

A structural note: frecency has two halves. The **modification** score is computed from file
mtime and git status and works automatically. The **access** score is stored and requires
someone to report that a file was opened — which a REST server never observes. fff warms the
database from git touch history at startup, so it is not empty, but the access half stays
largely inert unless a client calls `track-access`. Likewise the combo-boost
(`min_combo_count: 3`, multiplier 100) only fires if a client calls `track-query`.

Participation is therefore the client's choice. A client that never calls them loses nothing
relative to disabling the databases outright; one that does gets better ranking over time
with no server change.

## Errors

RFC 9457 `application/problem+json`, which ASP.NET Core consumes natively.

| Rust variant | Status |
|---|---|
| `InvalidPath`, `InvalidGlobPattern` | 400 |
| `FilesystemRoot` | 403 |
| `FilePickerMissing`, `WatcherNotReady`, `WatcherDisabled` | 503 |
| `DbInUse`, `EnvSpecMismatch` | 409 |
| `ThreadPanic` | 500 |

A malformed request body is `400 invalid-request-body`. This needs its own extractor:
`axum::Json`'s rejection is `text/plain`, which would leave the error a client is most likely
to hit during development outside the contract, while every other error deserialises as
`Problem`. `crate::extract::Json` wraps it and passes axum's field-level message through as
`detail`.

Each problem carries a stable machine-readable `code`; clients switch on that, never on the
prose `detail`. `fff_search::Error` is `#[non_exhaustive]`, so known variants are mapped
explicitly and a documented catch-all degrades a future variant to a generic 500.

## Configuration

TOML, with CLI flags and `FFF_SERVER_*` environment variables overriding.

```toml
[server]
bind = "0.0.0.0:8080"
# token = "..."                              # off by default

[workspaces]
create_block_ms = 10000                      # then 202 + poll
db_root = "C:/ProgramData/fff-server/db"      # per-workspace subdirectories

# Rescan interval = clamp(time_to_ready * duty_factor, min, max)
rescan_duty_factor = 25
rescan_min_secs = 60
rescan_max_secs = 1800

# Idle eviction = clamp(time_to_ready * idle_factor, min, max)
idle_factor = 120
idle_min_secs = 1800
idle_max_secs = 86400

[defaults]
ai_mode = true
content_indexing = true
watch = true
follow_symlinks = false
page_size = 100
grep_page_size = 50
# 5 s would cut a network grep off after ~700 of 87k files (measured ~7 ms/file over SMB).
# 0 disables the budget; clients page with cursors instead.
grep_time_budget_ms = 0

[logging]
level = "info"
json = false
```

Logging installs this server's own `tracing-subscriber` and does **not** call
`fff_search::log::init_tracing`. Because fff-core emits through the `tracing` facade, its
internal events are captured anyway — without inheriting its global one-shot subscriber, its
panic hook, or its file-naming scheme.

## Deployment

A plain console executable, wrappable as a Windows service with NSSM or `sc.exe`. No service
code in the binary, so it stays trivially runnable and debuggable from a terminal.

A **single-instance guard** is required, not optional: two processes indexing the same root
with the same database paths collide on LMDB.

There is no process-level idle shutdown — unlike `fff-mcp`, which is spawned per agent
session and should exit, this is a daemon other machines dial into. Per-workspace idle
eviction reclaims the memory without killing the process.

Binding `0.0.0.0` requires an inbound Windows firewall rule for the port; Windows blocks it
silently otherwise.

## Security posture

v1 runs in a lab behind a firewall: bound to all addresses, no token, no root allowlist. Both
the bind address and an optional bearer token are config-driven, and a root allowlist is a
config key that defaults to empty (permit all), so tightening any of this later is
configuration rather than a refactor.

Worth stating plainly: with no allowlist, any caller that can reach the port can make the
server index and grep any path the service account can read. fff refuses filesystem roots and
`$HOME` unless explicitly enabled, which narrows but does not close that.

## The .NET client

The contract is a committed, versioned `openapi.json`, also served at runtime. The generated
C# client belongs in the consuming solution, where its target framework, nullable-reference
settings, and naming conventions are already decided — a client generated here would import
guesses about all three. [Kiota](https://learn.microsoft.com/openapi/kiota/) is suggested for
modern .NET: cleaner `HttpClient`-based output, no Newtonsoft dependency.

An `openapi.json` snapshot test fails CI on any unintended contract change, so a contract
change is always a reviewed event rather than a runtime surprise downstream.

## Testing

Integration tests target **the server's own repository** — always present wherever tests run,
genuinely a git repo so git-status paths are exercised, and if the server cannot index its own
source it is broken. Against a live tree, assertions must be structural (*"at least one hit
whose `relativePath` ends in `Cargo.toml`"*, *"page 2 does not repeat page 1"*, *"every
`gitStatus.flags` value is a known variant"*) — never counts or orderings, which change as
the repo changes.

A small committed fixture tree under `tests/fixtures/` carries the assertions that must be
exact: a specific grep returning a specific line at a specific column with specific
`matchRangesUtf16`. That is what actually verifies the Rust → JSON translation, which is the
entire premise of the project.

Concurrency and eviction tests are out of scope for v1 — slow, flaky, and testing fff's
machinery more than this server's.

## Dependencies

`fff-search = "0.11"` from crates.io — a published stable, newer than the 0.10.6 in the
reference checkout at `d:\devel\fff`. A commented-out `[patch.crates-io]` stanza points at a
local path, so switching to a working copy for a debugging session is uncommenting two lines.

Default feature is `ripgrep` (pure Rust `ignore` + `globset`, no external toolchain). `zlob`
— fff's faster Zig-based walker and glob matcher, used by its own releases — is an opt-in
Cargo feature, not enabled by default: it requires Zig 0.16, and keeping `cargo build`
working with nothing but `rustup` matters more than walk speed on day one.

Single crate, DTOs in their own module. A separate types crate would serve a hypothetical
Rust client that is not on the roadmap; promoting the module later is mechanical.

## Known risks

- **git on a UNC root is untested.** Share-backed workspaces always carry a verbatim
  `\\?\UNC\` base path, and `path_utils::normalize` exists precisely to keep `base_path`
  byte-comparable with libgit2's `workdir()`. Whether that comparison survives a verbatim
  prefix is unknown — neither share tried held a git repo. If git status or `git_recency` is
  wanted on a share, test it before relying on it; a verbatim-prefix mismatch would most
  likely show up as `hasGitRepo: false` rather than as an error.
- **Grep over SMB is slow** — ~7 ms/file measured, so a full 87k-file grep is ~10 minutes.
  The default time budget is therefore disabled rather than set to a value that would
  silently truncate at ~700 files, and clients are expected to page with cursors.
- `relative_path_eq` uses a 512-byte buffer against a 4096-byte `PATH_BUF_SIZE`, so
  overflow-region files with very long relative paths cannot be looked up by path. Affects
  path lookup only, which v1 does not expose.
- `watcher/watch.rs` dispatch contains `.expect("watch event path must be inside the indexed
  base path")`, which panics the watcher thread rather than degrading. Reached only if a
  watcher event path is not byte-prefixed by `base_path`. v1 registers no watch subscriptions,
  which should keep this path cold; to be confirmed.
- `#[non_exhaustive]` on `fff_search::Error` means future variants degrade to a generic 500.

## Verified on Windows (step 0)

Measured with `spike/unc-probe` against `fff-search 0.11.0` on Windows 10 x64, MSVC,
rustc 1.98.1. All local-disk; the UNC column is what the lab run is for.

| Claim the design rests on | Result |
|---|---|
| MSVC builds the vendored C deps (libgit2, LMDB, memmap2) | ✅ clean, no toolchain config needed |
| `abort_signal` exists in published 0.11 | ✅ compiles and takes effect |
| `abort_signal` actually curtails work | ✅ 268 files/5240 matches/10.6 ms → 8/73/1.2 ms |
| `dunce` strips the `\\?\` prefix | ✅ `\\?\D:\devel\fff` → `D:\devel\fff` |
| Stored `base_path` is non-verbatim and round-trips | ✅ |
| Relative paths are `/`-canonical | ✅ 0 of 382 contained a backslash |
| `absolute_path` returns mixed separators | ✅ confirmed: `D:\devel\fff-server\spike/unc-probe/Cargo.toml` |
| `Score` has 13 fields incl. `git_recency_boost` | ✅ |
| Watcher delivers on local disk | ✅ new file indexed in ~500 ms |
| `wait_for_scan` implies watcher ready | ❌ **no** — scan 49 ms, indexing 319 ms, watcher 319 ms |

UNC results, measured in the lab against two trees on the same share:

| Claim | Result |
|---|---|
| UNC root indexes at all | ✅ 86,936 files |
| Mapped drive resolves to UNC unaided | ✅ `k:\users\…` → `\\?\UNC\…` |
| `dunce` strips `\\?\` on UNC | ❌ **no** — verbatim prefix is retained |
| Canonical path is a unique identity | ❌ **no** — UNC and `K:` spellings differ (host short vs FQDN, share casing) |
| Canonicalisation normalises casing | ✅ `stefano` → `Stefano` |
| Relative paths `/`-canonical on UNC | ✅ 0 of 86,936 |
| File I/O through a verbatim base path | ✅ grep read files and matched |
| Watcher delivers over SMB | ✅ **~500 ms**, same as local |
| `abort_signal` over SMB | ✅ 97 files → 8 |
| git discovery on a UNC root | ⬜ untested — no git repo on the shares tried |

Performance on the share (dominated by client-side antivirus, `MsMpEng`, not by the network):

| | 86,936 files | 159 files |
|---|---|---|
| Scan (searchable) | 59.5 s | 374 ms |
| Indexing complete | > 120 s (did not finish) | 7.76 s |
| Fuzzy search | **10.0 ms** | 293 µs |
| Grep | ~7.2 ms/file → full tree ≈ 10 min | 3.8 ms |

The fuzzy-search figures are the point of the whole exercise: 10 ms across 87k files on a
network share. Everything expensive is the one-time write path, which is exactly the cost a
long-lived daemon is for.

Two corrections these runs forced: content indexing is ~20× the walk (374 ms → 7.76 s), so
cost-derived intervals key off time-to-ready; and the watcher works fine over SMB, so nothing
special is done for network roots.

Two corrections this produced: `abort_signal` is coarse-grained (8 files still searched after
the flag was set), so cancellation is prompt rather than immediate; and the three readiness
stages are distinct, which is why workspace creation exposes all three.

`git_status` comes back `None` for clean files rather than an empty status — the convenience
`status` string is therefore `null` for clean files, not `"clean"`.

## Build order

0. ~~**UNC spike**~~ — **done**, see Verified. It changed four decisions: creation returns
   `202` rather than blocking, identity is by file ID rather than path string, rescan and
   eviction intervals derive from measured cost, and nothing is special-cased for network
   roots. `spike/unc-probe` is **retained** rather than deleted as originally planned: it
   is still the only way to answer the open git-on-UNC risk above, and costs nothing to keep
   (its own cargo workspace, excluded from the server build).
1. ~~Scaffold: crate, config, logging, `/v1/health`, `/openapi.json`, snapshot test.~~
   **Done.**
2. ~~Workspace pool: canonicalisation, file-ID dedupe, create/warm/status/evict,
   single-instance guard.~~ **Done.** Verified end to end: `D:/DEVEL/FFF-SERVER/` and
   `D:/devel/fff-server` resolve to one workspace id; a second instance over one `db_root`
   is refused; eviction stops the watcher and git worker cleanly.
3. Search routes and DTOs.
4. Grep routes: cursor pagination, cancellation, time budget.
5. Lifecycle, tracking, `parse-query`.
6. Fixture tests.
