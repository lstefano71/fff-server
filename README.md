# fff-server

A REST server over [fff](https://github.com/dmtrKovalenko/fff), exposing its file-search
engine as a typed HTTP API.

fff already ships an MCP server, but MCP returns **prose**. Its text formatter discards real
data on the way out: match highlight ranges, columns and byte offsets, fuzzy scores, parsed
locations, several result counts, and everything in the score breakdown beyond the total. It
also hardcodes about thirteen grep options and truncates output.

This server returns typed objects, specified as precisely as the engine's own Rust API
allows, for programs rather than language models.

Public domain — see [LICENSE](LICENSE). The reasoning behind every design decision, and the
measurements behind them, are in [DESIGN.md](DESIGN.md).

## Quickstart

```powershell
cargo run --release -- --bind 127.0.0.1:8080
```

```bash
# Index a root. Idempotent: any spelling of the same directory returns one workspace.
curl -X POST http://127.0.0.1:8080/v1/workspaces \
  -H 'content-type: application/json' \
  -d '{"root":"D:/devel/my-project","waitFor":"indexing"}'
# -> {"id":"e0121f1905da56a9","status":"ready","indexedFiles":382, ...}

# Fuzzy path search
curl -X POST http://127.0.0.1:8080/v1/workspaces/<id>/search \
  -H 'content-type: application/json' \
  -d '{"query":"src/**/*.rs button","pageSize":20}'

# Content search
curl -X POST http://127.0.0.1:8080/v1/workspaces/<id>/grep \
  -H 'content-type: application/json' \
  -d '{"query":"*.rs TODO","afterContext":1,"classifyDefinitions":true}'
```

The contract is at `GET /openapi.json`, and committed as [openapi.json](openapi.json).

## Endpoints

| | |
|---|---|
| `POST/GET/DELETE /v1/workspaces[/{id}]` | Create or warm, list, inspect, evict |
| `POST /v1/workspaces/{id}/search` | Fuzzy file search |
| `POST /v1/workspaces/{id}/search/directories` | Directories |
| `POST /v1/workspaces/{id}/search/mixed` | Files and directories, tagged |
| `POST /v1/workspaces/{id}/glob` | Literal glob, frecency-ranked |
| `POST /v1/workspaces/{id}/grep` | Content search: plain, regex, fuzzy, auto |
| `POST /v1/workspaces/{id}/multi-grep` | Many literal patterns at once |
| `POST /v1/workspaces/{id}/rescan` | Force a rescan |
| `POST /v1/workspaces/{id}/git/refresh` | Refresh cached git status |
| `POST /v1/workspaces/{id}/track-access` | Report a file was opened |
| `POST /v1/workspaces/{id}/track-query` | Report which result a query led to |
| `GET /v1/workspaces/{id}/history` | Query history |
| `POST /v1/parse-query` | Show how the query DSL decomposed |
| `GET /v1/health` | Liveness and workspace count |

## What you get that the other bindings drop

- **The complete score breakdown** — all 13 fields. The C ABI drops `gitStatusBoost`; the
  TypeScript SDK carries only 10, missing that plus `pathAlignmentBonus` and the
  `gitRecencyBoost` fff 0.11 added.
- **Match ranges, in UTF-16 code units.** The engine emits UTF-8 byte offsets and .NET
  strings are UTF-16, so a byte offset would address the wrong characters on any non-ASCII
  line. Converted server-side and named for its units.
- **Complete git status.** `git2::Status` is bitflags — a file can be staged-modified *and*
  worktree-modified at once. Both a convenience label and the full `flags` array are emitted.
- **Parsed locations.** A trailing `file.rs:42:10` comes back as structured data.
- **Real cancellation.** A disconnected client aborts an in-flight grep rather than leaving
  it to run for a response nobody will read.
- **Multi-root.** The MCP server indexes one root, fixed at startup. This holds a pool,
  deduplicated by filesystem identity so a UNC path and a mapped drive pointing at the same
  directory give one index, not two.

## Generating a client

The OpenAPI document is generated from the Rust types, so it cannot drift from the engine.
Generate the client in your own solution, where the target framework and naming conventions
are already decided.

Three generators were tried against this contract, and the output compiled, rather than
assumed to work. Results:

| Generator | Result | Unions become |
|---|---|---|
| **[Refitter](https://github.com/christianhelle/refitter)** | Clean generate, clean build | Base class + derived types + `JsonInheritanceConverter` on `type` |
| **[Kiota](https://learn.microsoft.com/openapi/kiota/)** | Clean generate, clean build, no warnings | Composed-type wrapper with one nullable property per variant |
| **NSwag** (CLI, defaults) | Does not compile | Emits `ICollection<Constraints>` / `ICollection<Items>` without defining those types |

**Refitter is the recommendation.** It is a single `dotnet tool`, needs only Refit and
`System.Text.Json` with no proprietary runtime, produces a DI-friendly interface, and turns
the discriminated unions into ordinary C# polymorphism:

```bash
dotnet tool install --global Refitter
refitter openapi.json --namespace FffClient --output ./FffClient/FffClient.cs
dotnet add package Refit
```

```csharp
foreach (var hit in response.Items)          // ICollection<MixedHit>
    if (hit is MixedFileHit file)
        Console.WriteLine(file.Item.RelativePath);
```

Kiota is a perfectly good alternative and is also verified:

```bash
kiota generate -l CSharp -d http://localhost:8080/openapi.json -o ./FffClient -c FffClient
dotnet add package Microsoft.Kiota.Bundle
```

NSwag's failure is in its own generation, not in the contract, and is likely fixable with
generation-mode flags — not pursued, since two generators already work.

The contract carries an absolute server url, so a generated client's base address is set for
you.

### Shapes

`ConstraintDto` and `MixedHit` are proper discriminated unions: a `oneOf` of named variant
schemas plus an OpenAPI `discriminator` on `type`. That is what makes them generatable —
utoipa only emits a discriminator for an enum whose variants are newtypes over *named*
schemas, and an anonymous `oneOf` is something generators either refuse or mis-deserialise.

`LocationDto` is deliberately flat, because it is the only union that appears as an optional
field and nesting a union inside `oneOf: [null, $ref]` loses the inheritance relationship
Kiota needs. See DESIGN.md.

A snapshot test fails CI on any unintended contract change, and a further test asserts the
unions keep their discriminator, mapping, and a required `type` on every variant — so a
change that would break your codegen shows up as a failing test rather than in your build
output.

## Configuration

Defaults, then `fff-server.toml`, then `FFF_SERVER_*` environment variables, then CLI flags.
Every key and its rationale is in [fff-server.toml](fff-server.toml); `--print-config` dumps
the effective result.

Two things worth knowing before deploying:

- The default bind is `0.0.0.0:8080` with **no authentication** — intended for a lab behind a
  firewall. Any caller that can reach the port can index and grep any path the process can
  read. A bearer token and a root allowlist are both config keys.
- Binding all interfaces needs an inbound Windows firewall rule for the port; Windows blocks
  it silently otherwise.

Run it as a plain console executable, or wrap it with NSSM or `sc.exe` for boot persistence.
A single-instance guard refuses to start twice over one database root, because two servers
would collide on LMDB.

## Building

Needs a Rust toolchain; on Windows use the **MSVC** host (`x86_64-pc-windows-msvc`), since the
build compiles libgit2, LMDB and mimalloc from source. No other toolchain setup is required —
`cc` locates MSVC itself.

```bash
cargo build --release
cargo test
```

`zlob`, fff's faster Zig-based walker, is available as an opt-in Cargo feature and is not
enabled by default: it needs Zig 0.16, and keeping `cargo build` working with nothing but
`rustup` matters more than walk speed.

## Tests

- `tests/fixture_tree.rs` asserts exact values against a small committed tree, including a
  file whose lines put APL glyphs, an em dash and an astral-plane emoji before the match, so
  the UTF-16 conversion is verified rather than assumed.
- `tests/own_repo.rs` indexes this repository itself. Assertions there are structural only —
  never counts or orderings — because the target changes as work proceeds.
- `tests/openapi_snapshot.rs` guards the published contract. Regenerate deliberately with
  `UPDATE_OPENAPI=1 cargo test`.
