# kitchen-sink

A full-surface agentgear host, meant to be copied and trimmed: every plugin
component type (MCP server, hooks, a command, a subagent, a skill), wired across
seven harnesses (`claude` plus six non-CC backends). See
[`hello-mcp`](../hello-mcp) for the minimal shape this expands on.

## What it demonstrates

- The full `PluginHost` lifecycle: `setup` / `update` / `uninstall` / `self-heal` /
  `check-restart` / `doctor`, plus the binary's own `mcp` subcommand.
- `install_into` with a `--agent` filter, so `setup` can target one backend without
  touching the rest.
- `SessionStart` -> `self-heal` and `UserPromptSubmit` -> `check-restart` hooks,
  translated per-backend (e.g. gemini maps `UserPromptSubmit` to its own
  `BeforeAgent` event).
- Merge safety: `tests/lifecycle.rs` seeds a foreign config entry per backend and
  asserts it survives both install and uninstall.
- A dependency-free stdio MCP server (`kitchen-sink mcp`) the plugin tree points
  every harness's MCP entry at, so each one gets a live server to register.

## Files

| path | role |
|---|---|
| `Cargo.toml` | six non-CC features enabled (`codex`, `opencode`, `gemini`, `cursor`, `crush`, `goose`) alongside the default `derive` + `claude` + `embed` |
| `build.rs` | pins `plugin.json` `version` to `CARGO_PKG_VERSION` |
| `plugin/.claude-plugin/plugin.json` | plugin metadata; its MCP entry points back at `kitchen-sink mcp` |
| `plugin/hooks/hooks.json` | `SessionStart` -> `self-heal`, `UserPromptSubmit` -> `check-restart` |
| `plugin/commands/greet.md` | one slash command |
| `plugin/agents/reviewer.md` | one subagent |
| `plugin/skills/demo/SKILL.md` | one skill (Claude Code only; no non-CC backend translates skills yet) |
| `src/main.rs` | `setup` / `update` / `uninstall` / `self-heal` / `check-restart` / `doctor` / `mcp` |
| `tests/lifecycle.rs` | hermetic gemini + crush install/uninstall against a temp `HOME`, each including the merge-safety case |

## Run it

```console
$ cargo build -p kitchen-sink
$ cargo test -p kitchen-sink
```

Every example below runs the built binary directly against a scratch `HOME` (+ a
`PATH` holding only the binary's own dir, so no real agent CLI on your machine gets
detected): nothing here touches your actual config. Build first; `cargo run` under
a scratch `HOME` breaks rustup's own toolchain lookup.

```console
$ cargo build -p kitchen-sink
$ BIN=target/debug/kitchen-sink
$ HOME=$(mktemp -d) PATH=$(dirname "$BIN") "$BIN" doctor
[ ok ] host binary on PATH: `kitchen-sink` resolves on PATH
[ ok ] claude: not installed on this host; skipped
[ ok ] codex: not installed on this host; skipped
[ ok ] opencode: not installed on this host; skipped
[ ok ] gemini: not installed on this host; skipped
[ ok ] cursor: not installed on this host; skipped
[ ok ] crush: not installed on this host; skipped
[ ok ] goose: not installed on this host; skipped
$ echo $?
0
```

A harness with no CLI and no config dir present is not a failure, just out of
scope for that host. Pre-creating `~/.gemini` is enough for detection (no `gemini`
binary needed), matching what `tests/lifecycle.rs` does:

Set every var in one `env` call per command; splitting `HOME=... ; export ...`
across lines is an easy way to forget to export one of them, which silently falls
back to your real `HOME` for that var:

```console
$ ROOT=$(mktemp -d); mkdir -p "$ROOT/.gemini" "$ROOT/data" "$ROOT/run"
$ E() { env HOME="$ROOT" XDG_DATA_HOME="$ROOT/data" XDG_RUNTIME_DIR="$ROOT/run" PATH="$(dirname "$BIN")" "$BIN" "$@"; }
$ E setup --agent gemini
Installed
$ cat "$ROOT/.gemini/settings.json"
{
  "mcpServers": { "kitchen-sink": { "command": "kitchen-sink", "args": ["mcp"], "env": {} } },
  "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "kitchen-sink self-heal" }] }],
    "BeforeAgent":  [{ "hooks": [{ "type": "command", "command": "kitchen-sink check-restart" }] }]
  }
}
$ E check-restart   # silent: no update pending yet
$ E uninstall
Removed
$ cat "$ROOT/.gemini/settings.json"
{ "mcpServers": {}, "hooks": {} }
```

(`settings.json` reformatted for the README; the real output is unindented single
lines. Output captured verbatim otherwise.)

The `mcp` subcommand is the plugin's own server, speaking newline-delimited
JSON-RPC on stdio:

```console
$ printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}\n' | "$BIN" mcp
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"kitchen-sink","version":"0.1.0"}}}
{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}
```

## Caution outside a scratch `HOME`

`setup` with no `--agent` filter installs into every harness it detects on the
current machine, not just Claude Code. If you have `gemini`/`cursor`/`crush`/etc.
config directories under your real `HOME`, a bare `kitchen-sink setup` registers
the plugin in all of them. Use `--agent <id>` to scope it during testing.
