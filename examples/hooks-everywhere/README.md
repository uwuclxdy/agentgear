# hooks-everywhere

A hooks-only agentgear host: no MCP server, no commands, no agents, no skills.
[`hello-mcp`](../hello-mcp) is the smallest host to start from; this one exists to
show hook translation. Read `plugin/hooks/hooks.json` for the bindings the derive
fans out, then `src/main.rs` for the handlers behind them.

The plugin ships four hook bindings and fans them out across the 14 harnesses its
derive names, one of them Claude Code itself:

| CC event | matcher | hook target |
|---|---|---|
| `SessionStart` | - | `hooks-everywhere self-heal` |
| `UserPromptSubmit` | - | `hooks-everywhere check-restart` |
| `PreToolUse` | `Bash` | `hooks-everywhere guard` |
| `PostToolUse` | - | `hooks-everywhere audit` |

`guard` and `audit` read the hook's JSON payload from stdin without parsing it,
optionally append one line to `$HOOKS_EVERYWHERE_LOG`, and always exit 0. See the
module doc on `src/main.rs` for why a hook body has to stay that boring.

## Run it

```console
$ cargo build -p hooks-everywhere
$ HOME=/tmp/scratch-home cargo run -p hooks-everywhere -- setup --agent gemini
Installed
$ cat /tmp/scratch-home/.gemini/settings.json
```

`setup` (no `--agent`) installs into every backend it detects on the machine;
repeat `--agent <id>` to narrow it. `doctor` reports per-backend health,
`uninstall` removes exactly what this plugin wrote.

## Which event lands where

Derived from each backend's own `map_event` (`crates/agentgear/src/agents/<id>.rs`).
A checkmark means the CC event has a translated analog in that harness's own hook
system; a dash means agentgear skips it rather than writing it under a guessed name.

| harness | `SessionStart` | `UserPromptSubmit` | `PreToolUse` | `PostToolUse` |
|---|---|---|---|---|
| claude | Y | Y | Y | Y |
| codex | Y | Y | Y | Y |
| gemini\* | Y | Y (`BeforeAgent`) | Y\* (`BeforeTool`) | Y\* (`AfterTool`) |
| cursor | Y (`sessionStart`) | Y (`beforeSubmitPrompt`) | Y (`preToolUse`) | Y (`postToolUse`) |
| cline\* | - | Y | Y\* | Y\* |
| devin | Y | Y | Y | Y |
| qwen-code | Y | Y | Y | Y |
| kimi | Y | Y | Y | Y |
| goose | Y | Y | Y | Y |
| crush | - | - | Y | - |
| droid | Y | Y | Y | Y |
| augment | Y | Y (`PromptSubmit`) | Y | Y |
| antigravity-cli\* | - | Y (`PreInvocation`) | Y\* | Y\* |

`copilot-cli` is not in this table: it's plugin-native, so the whole
`hooks/hooks.json` copies into `~/.copilot/installed-plugins/` verbatim, CC event
names included, rather than going through a per-event `map_event`. Whether the
copied file actually fires is unconfirmed (no headless hooks-list command
exists), so it carries no `Y`/`-` verdict here.

agentgear ships 15 hook-capable backends, one more than this host's derive names.
The odd one out is `vscode-copilot`, which declares `scopes: &["project"]` and so
writes at project scope only; this host installs at `Scope::User`, where the
fan-out skips it. A host reaches it by naming `vscode-copilot` in its derive's
`agents = [...]` and installing at `Scope::Project { path }`, since the fan-out
iterates the derive's agent list before it ever looks at scope.

crush is the one harness (besides copilot-cli) that skips most of this plugin's
surface outright. It defines exactly one hook event (`PreToolUse`), so only
`guard` lands there. `self-heal`, `check-restart`, and `audit` are never written
under a guessed name. That ceiling is crush's own: its source declares a single
hook-event constant (`EventPreToolUse`, `internal/hooks/hooks.go`), and no other
event name is dispatched anywhere in its agent loop.

\* This plugin's `guard` hook is scoped to CC's `Bash` tool, and a tool matcher
does not survive translation everywhere. Two things happen to it. gemini and
antigravity-cli take the CC tool name verbatim into a harness whose own tools
are named differently (`run_command`, not `Bash`), so the hook lands correctly
and then matches nothing. cline has no matcher field at all, so `guard` runs on
*every* tool there instead. Either way the hook itself is live; only its
scoping is lost, and the unmatched events above are unaffected. Per-harness
detail: [Agent backends](https://github.com/uwuclxdy/agentgear/wiki/Agent-Backends#hook-event-mapping).

A `Y` is what a backend's `map_event` translates to, not proof the hook fires in
a live session: several harnesses need auth this example's tests do not have.
Every name written is one the harness's own docs or binary carry, though, so a
`Y` never means a guessed event. (kiro left this table 2026-07-17: its only hook
surface is a user-owned per-agent config, so the kiro backend now declares hooks
unsupported.)

## Tests

`tests/lifecycle.rs` drives `hooks-everywhere setup --agent <id>` against a temp
`HOME` (plus each backend's own override env) for codex, gemini, kimi, droid, and
crush, with no docker, no auth, and no real harness binary. Each test asserts the
exact translated hook shape, that a pre-seeded foreign hook survives both install
and uninstall, and (crush only) that the three unmapped events are never written
at all.

```console
$ cargo test -p hooks-everywhere
running 5 tests
test codex_all_events_land ... ok
test gemini_all_events_land ... ok
test kimi_all_events_land ... ok
test droid_all_events_land ... ok
test crush_only_pretooluse_lands ... ok
```
