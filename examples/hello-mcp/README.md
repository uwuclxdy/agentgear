# hello-mcp

The smallest real agentgear host: one derived struct, a one-line `build.rs`, and a
`setup` subcommand that ships a single stdio MCP server to Claude Code. No non-CC
backends, no `update` / `self_heal` / `check-restart`. See
[`kitchen-sink`](../kitchen-sink) for a host that wires the full lifecycle across
seven harnesses.

## What it demonstrates

- `#[derive(PluginHost)]` with no `agents = [...]` attribute, which targets Claude
  Code only.
- The plugin tree (`plugin/`) embedded into the binary at compile time via the
  default `embed` feature, so `setup` installs offline from the built binary.
- A three-command binary: `setup`, `uninstall`, `doctor`.

## Files

| path | role |
|---|---|
| `Cargo.toml` | default features only (`derive` + `claude` + `embed`) |
| `build.rs` | pins `plugin.json` `version` to `CARGO_PKG_VERSION` |
| `plugin/.claude-plugin/plugin.json` | plugin metadata + one `mcpServers` entry |
| `src/lib.rs` | the `HelloMcp` derive struct, exercised by `tests/wiring.rs` |
| `src/main.rs` | `setup` / `uninstall` / `doctor` |
| `tests/wiring.rs` | asserts the derive + embed wiring compiled correctly |

## Run it

```console
$ cargo build -p hello-mcp
$ cargo test -p hello-mcp
```

`setup` and `uninstall` run the real `claude` CLI (≥ 2.1.196) and mutate whatever
Claude Code registry `HOME` points at, so this README shows the commands without
running them against your config:

```console
$ target/debug/hello-mcp setup      # claude plugin marketplace add + install
$ target/debug/hello-mcp uninstall  # reverses it
```

`doctor` only reads state, so it is safe to run for real. Build first, then run the
binary directly against a scratch `HOME`. `cargo run` under a scratch `HOME` breaks
rustup's own toolchain lookup, since rustup resolves its default toolchain from
`HOME` too:

```console
$ cargo build -p hello-mcp
$ HOME=$(mktemp -d) target/debug/hello-mcp doctor
[fail] host binary on PATH: `hello-mcp` is not on PATH, so the plugin's hooks cannot invoke it
       fix: install the binary into a PATH directory (e.g. `cargo install` or a package)
[ ok ] claude version: 2.1.211 (Claude Code)
[fail] plugin registered: hello-mcp@hello-mcp is not installed
       fix: run the host binary's `setup` (or `install`) subcommand
[warn] manifest validates: no materialized tree to validate (embedded tree not yet materialized)
[warn] current tree matches embedded: nothing materialized yet
[ ok ] hook commands on PATH: all referenced bare commands resolve
$ echo $?
1
```

(captured against a real `claude` 2.1.211 on `PATH`, nothing installed yet; your
version and exit path will differ). `doctor` exits non-zero if any check fails.
A real `setup` flips "plugin registered" to `ok` and materializes the tree so
"manifest validates" / "current tree matches embedded" pass too. Putting the binary
on `PATH` (`cargo install` or packaging) clears the first check.

## Minimal on purpose

This host only wires `setup` / `uninstall` / `doctor`. `update`, `self_heal` (the
`SessionStart` hook target), and `check-restart` (the `UserPromptSubmit` hook
target) are real `PluginHost` methods this crate never calls. It ships no hooks,
so nothing invokes them. [`kitchen-sink`](../kitchen-sink) wires all seven.
