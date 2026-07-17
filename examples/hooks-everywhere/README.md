# hooks-everywhere

A hooks-only agentgear host: no MCP server, no commands, no agents, no skills.
The plugin ships four hook bindings and fans them out across every hook-capable
harness agentgear supports (14 in the derive, one of them Claude Code itself):

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
| gemini | Y | Y (`BeforeAgent`) | Y (`BeforeTool`) | Y (`AfterTool`) |
| cursor | Y | Y (`beforeSubmitPrompt`) | Y | Y |
| cline | - | Y | Y | Y |
| devin | Y | Y | Y | Y |
| qwen-code | Y | Y | Y | Y |
| copilot-cli | Y (`sessionStart`) | Y (`userPromptSubmitted`) | Y | Y |
| kimi | Y | Y | Y | Y |
| goose | Y | Y | Y | Y |
| crush | - | - | Y | - |
| droid | Y | Y | Y | Y |
| augment\*\* | Y | - | Y | Y |
| antigravity-cli\* | Y (`BeforeAgent`) | Y | Y | Y |

crush is the one harness that skips most of this plugin's surface outright. It
defines exactly one hook event (`PreToolUse`), so only `guard` lands there.
`self-heal`, `check-restart`, and `audit` are never written under a guessed name.
That is the real, verified shape of crush's own hook engine
(`docs/research/verify-crush.md`), not a translation gap.

\* antigravity-cli has known landing bugs tracked in `docs/todo.md` §0: two
illegal event names plus a wrong target file. The `Y`s above are what each
backend's `map_event` currently translates to, not proof the hook actually
fires yet. This example's hermetic tests only exercise codex/gemini/kimi/crush,
so antigravity-cli does not land in `tests/lifecycle.rs`. (kiro left this table
2026-07-17: its only hook surface is a user-owned per-agent config, so the kiro
backend now declares hooks unsupported.)

\*\* augment currently drops `UserPromptSubmit` outright (`docs/todo.md` §0: it
has a live `PromptSubmit` analog the backend does not map yet).

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
