# fff-server PowerShell client

`Fff.psm1` is a small PowerShell client for a running
[fff-server](../../README.md). It returns ordinary PowerShell objects, so results can be
filtered, sorted, grouped, exported, or opened without parsing formatted command output.

## Quick start

Start the server on the machine that can read the repository:

```powershell
cd D:\devel\fff-server
.\target\release\fff-server.exe --bind 0.0.0.0:8080
```

On the client machine, import the module and connect:

```powershell
Import-Module D:\path\to\Fff.psm1

Connect-FffServer `
    -BaseUrl http://s81alfa01:8080 `
    -Root 'D:\Trash\sofia\sofia81alfa00'
```

`-Root` is interpreted by the **server**, not the client. Use a path that exists from the
server process's point of view. If authentication is enabled, pass the configured bearer
token with `-Token`.

The connection and current workspace are retained in the imported module for the rest of
the PowerShell session.

## Everyday commands

| Short name | Full command | Purpose |
|---|---|---|
| `ff` | `Find-FffFile` | Fuzzy file search |
| `fg` | `Find-FffText` | Search file contents |
| `fd` | `Find-FffDirectory` | Fuzzy directory search |
| | `Find-FffGlob` | Exact glob-based file selection |
| | `Open-FffFile` | Open a result and update its frecency |

### `ff`: fuzzy file search

`ff` searches paths and filenames. Terms do not need to be exact, and constraints can be
mixed with fuzzy words:

```powershell
ff 'customer controller'
ff 'src/**/*.cs customer controller'
ff 'git:modified'
ff 'git:modified src/**/*.cs !tests/ customer'
```

Glob separators inside a query must be `/`, even on Windows. Use `Test-FffQuery` when a
query produces surprising results.

Useful parameters:

```powershell
ff 'controller' -Take 100
ff 'controller' -Page 2 -Take 100
ff 'controller' -All
ff 'controller' -CurrentFile 'src/current/file.cs'
```

`-CurrentFile` deprioritizes the file already being viewed and gives nearby paths a ranking
boost. `-All` requests pages until the result set is exhausted.

Each result is an `Fff.FileHit` object with these commonly useful properties:

| Property | Meaning |
|---|---|
| `Path` | Workspace-relative path using `/` |
| `FullPath` | Native absolute path suitable for opening |
| `Name` | Filename |
| `Score` | Total search score |
| `Match` | Match type, such as `exact`, `prefix`, or `fuzzy_filename` |
| `Git` / `GitFlags` | Convenient status plus all underlying git status flags |
| `Size` | File size in bytes |
| `Modified` | Last-modified `DateTime` |
| `Frecency` / `GitRecency` | Usage/modification and git-history ranking inputs |
| `Ranges` | UTF-16 ranges that matched within `Path` |
| `Breakdown` | Complete score breakdown returned by the engine |

Because these are objects, normal PowerShell operations work:

```powershell
ff '*.cs' -All |
    Where-Object Size -gt 100kb |
    Sort-Object Size -Descending |
    Select-Object -First 20 Path, Size, Git

ff 'git:modified' -All | Export-Csv .\modified-files.csv -NoTypeInformation
ff 'controller' -Take 200 | Out-GridView
```

### `fg`: content search

`fg` returns one `Fff.TextHit` object per matching line:

```powershell
fg 'TODO'
fg '*.cs TODO'
fg '*.cs !tests/ TODO'
fg 'catch\s*\(' -Mode Regex
fg 'IsDateTimePicker' -Mode Fuzzy
```

The default `Auto` mode chooses literal or regular-expression matching based on the query.
Available modes are `Auto`, `Plain`, `Regex`, and `Fuzzy`. Fuzzy mode needs content
indexing to be complete.

Useful parameters:

```powershell
fg 'TODO' -Casing Sensitive
fg 'TODO' -Context 2
fg 'class Customer' -Definitions
fg 'TODO' -Take 200 -All
fg 'TODO' -TimeBudgetMs 5000
```

- `-Casing` accepts `Smart`, `Sensitive`, or `Insensitive`.
- `-Context N` includes N lines before and after each match.
- `-Definitions` asks the server to classify definition-like matches.
- `-All` follows continuation cursors until the search is exhausted.
- `-TimeBudgetMs` limits each server request. Zero means unbounded.

Pipe results through `Format-FffMatch` for readable, highlighted output:

```powershell
fg 'TODO' -Context 1 | Format-FffMatch
fg 'TODO' | Format-FffMatch -NoColour
```

Highlight ranges use UTF-16 offsets, the same indexing used by .NET strings, so highlighting
remains correct when a line contains APL glyphs, accented text, or emoji.

Useful `Fff.TextHit` properties include:

| Property | Meaning |
|---|---|
| `Path` / `FullPath` | Relative and absolute file paths |
| `Line` / `Col` | One-based line and UTF-16 column |
| `Text` | Matching line |
| `Ranges` | All matched spans in the line |
| `Before` / `After` | Requested context lines |
| `IsDef` | Whether the line looks like a definition |
| `Git` | File git status |
| `ByteOffset` | Byte position of the line in the file |

Examples using the objects directly:

```powershell
fg 'TODO' -All | Group-Object Path | Sort-Object Count -Descending
fg 'obsolete' -All | Select-Object Path, Line, Text | Export-Csv .\obsolete.csv
fg 'CustomerService' | Select-Object -First 1 | Open-FffFile -Editor code
```

### `fd`: fuzzy directory search

```powershell
fd 'customer'
fd 'migration' -Take 100
```

Results contain `Path`, `Name`, `Score`, `Frecency`, and `FullPath`.

### `Find-FffGlob`: literal glob search

Use this when the input is already a glob and should not also be interpreted as fuzzy text:

```powershell
Find-FffGlob '**/*.cs'
Find-FffGlob 'src\**\*.cs' -All
```

Unlike raw `ff` and `fg` queries, this command safely converts Windows `\` separators to
`/` before sending the pattern.

## Opening results

`Open-FffFile` accepts file or text hits from the pipeline:

```powershell
ff 'customer controller' | Select-Object -First 1 | Open-FffFile
fg 'TODO' | Select-Object -First 1 | Open-FffFile -Editor code
```

Without `-Editor`, PowerShell uses the Windows file association. With `-Editor code`, text
hits open in VS Code at the matching line. Any executable available on `PATH` can be passed
as the editor.

Opening a result normally reports the access to the server. This feeds fff's frecency
ranking, allowing frequently opened files to rise in later searches. Use `-NoTrack` to skip
that update.

## Connection and maintenance

```powershell
Get-FffHealth
Get-FffWorkspace
Update-FffIndex
Update-FffIndex -Git
```

- `Get-FffHealth` checks the current server.
- `Get-FffWorkspace` returns readiness, counts, git information, and rescan/eviction timing.
- `Update-FffIndex` starts a filesystem rescan.
- `Update-FffIndex -Git` refreshes cached git status after a commit or branch switch.

`Connect-FffServer` has three readiness choices:

| `-WaitFor` | Returns when |
|---|---|
| `Scan` | Files are available for path search |
| `Indexing` | Content indexing is complete; this is the default |
| `Watcher` | Content indexing is complete and live filesystem updates are active |

For a large repository, `Scan` gives the fastest path to `ff`; use the default `Indexing`
before relying on fuzzy `fg`. `-TimeoutSeconds` controls how long the client polls, while
`-Quiet` suppresses connection and progress messages.

## Understanding the query DSL

A query can combine constraints and fuzzy/search text:

```text
git:modified src/**/*.cs !src/**/Generated/*.cs customer controller
```

This means:

- include modified files;
- include `.cs` files below `src`;
- exclude generated `.cs` files;
- rank the remaining paths against `customer controller`.

Raw `ff` and `fg` queries are passed to the server unchanged. In particular, do not use
Windows backslashes in glob-looking query tokens:

```powershell
Test-FffQuery 'src\**\*.cs'  # warns
Test-FffQuery 'src/**/*.cs'  # parsed as a glob constraint
```

For grep, backslashes may be intentional regex or source text, so the module cannot safely
rewrite the whole query.

## Getting command help

The module also includes PowerShell help:

```powershell
Get-Help Connect-FffServer -Full
Get-Help ff -Full
Get-Help fg -Full
Get-Help Open-FffFile -Examples
```

To run the dependency-free client regression checks:

```powershell
pwsh -NoProfile -File .\Fff.Tests.ps1
```
