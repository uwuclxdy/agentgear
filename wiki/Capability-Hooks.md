# Hooks

A hook is a command the harness runs on a lifecycle event (a tool call, a prompt submit, session
start, …). The plugin declares them in Claude Code's shape —
`{hooks:{Event:[{matcher?, hooks:[{type:"command", command}]}]}}` — and agentgear translates each
binding into the target tool's own hook config, renaming the event where the tool uses different
names.

**15 backends host hooks.** The other 10 have no writable hook surface: some accept only JS/TS
module hooks (opencode, openclaw, omp, pi), some have no usable hook key (amp, kilo,
jetbrains-copilot, antigravity the IDE; zed's only hook is a one-variant `create_worktree` enum),
and kiro hosts hooks only inside user-owned agent files, which agentgear will not edit.

## Where each hook lands

| harness | file | entry shape | trust gate |
|---|---|---|---|
| claude | `hooks/hooks.json` (plugin root) | `{hooks:{Event:[{matcher?,hooks:[{type:"command",command}]}]}}` | none: plugin install is the gate |
| antigravity-cli | `~/.gemini/config/hooks.json` (user) · `<root>/.agents/hooks.json` (project) | `{"<plugin>":{"<Event>":[…]}}`; tool events grouped `{matcher,hooks:[…]}`, others flat | folder trust (project scope) |
| augment | `~/.augment/settings.json` `hooks.<Event>` | `[{matcher?,hooks:[{type:"command",command}]}]` | none, fires immediately |
| cline | `<store>/Hooks/<Event>` (user) · `.clinerules/hooks/<Event>` (project) | event-named executable, no extension; stdin = event JSON | none found; scanned once present + executable |
| codex | `hooks.json` | CC's exact shape | `/hooks` TUI content-hash trust, else `--dangerously-bypass-hook-trust`. **inert until then** |
| copilot-cli | native — `hooks/hooks.json` copies into the tree verbatim, CC's shape | CC's exact shape, event names untouched | whether the copied file fires is unconfirmed (no headless hooks-list) |
| crush | `crush.json` root `hooks.PreToolUse[]` | flat `{command,matcher?}` | none. an `allow` decision pre-approves the permission prompt |
| cursor | `hooks.json` | flat `{command,matcher?}` per event array | none, both scopes live |
| devin | `config.json` `hooks.<Event>` | CC shape verbatim | loads on presence; execution gate untested |
| droid | `hooks.json` | `{hooks:{Event:[{matcher?,hooks:[{type:"command",command}]}]}}` | none upfront; a running session needs `/hooks` review or a restart |
| gemini | `settings.json` `hooks.<Event>` | `[{matcher?,hooks:[{type:"command",command}]}]` | project blocked in an untrusted folder, user unconditional; plus a name+command re-consent |
| goose | `~/.agents/plugins/<plugin>/hooks/hooks.json` | CC's exact shape | none: auto-discovered, auto-enabled |
| kimi | `~/.kimi-code/config.toml` `[[hooks]]` | `{event,matcher?,command,timeout?}` | none. live-fire proven; the only trust gate is `/plugins install` |
| qwen-code | `settings.json` `hooks.<Event>` | `[{matcher?,hooks:[{type:"command",command}]}]`, CC-identical | project needs folder trust, user unconditional |
| vscode-copilot | `.github/hooks/<plugin>.json` | `{hooks:{Event:[{type:"command",command}]}}`, CC `matcher` dropped | none, loads on a default `chat.hookFilesLocations` path |

## Event map

Most hook-capable backends reuse Claude Code's PascalCase event names, so agentgear maps them 1:1
and skips events with no analog. The renames and gaps:

| CC event | claude | codex | gemini | cursor | cline | devin | qwen-code | vscode-copilot | kimi | antigravity-cli | goose | crush | droid | augment |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| SessionStart | SessionStart | SessionStart | SessionStart | sessionStart | — | SessionStart | SessionStart | SessionStart | SessionStart | — | SessionStart | — | SessionStart | SessionStart |
| SessionEnd | SessionEnd | — | SessionEnd | sessionEnd | SessionShutdown | SessionEnd | SessionEnd | SessionEnd | SessionEnd | — | SessionEnd | — | SessionEnd | SessionEnd |
| UserPromptSubmit | UserPromptSubmit | UserPromptSubmit | BeforeAgent | beforeSubmitPrompt | UserPromptSubmit | UserPromptSubmit | UserPromptSubmit | UserPromptSubmit | UserPromptSubmit | PreInvocation | UserPromptSubmit | — | UserPromptSubmit | PromptSubmit |
| PreToolUse | PreToolUse | PreToolUse | BeforeTool ⚠️ | preToolUse | PreToolUse ᵈ | PreToolUse | PreToolUse | PreToolUse ᵈ | PreToolUse | PreToolUse ⚠️ | PreToolUse | PreToolUse | PreToolUse | PreToolUse |
| PostToolUse | PostToolUse | PostToolUse | AfterTool ⚠️ | postToolUse | PostToolUse ᵈ | PostToolUse | PostToolUse | PostToolUse ᵈ | PostToolUse | PostToolUse ⚠️ | PostToolUse | — | PostToolUse | PostToolUse |
| Stop | Stop | Stop | — | stop | — | Stop | Stop | Stop | Stop | Stop | Stop | — | Stop | Stop |
| SubagentStart | SubagentStart | SubagentStart | — | subagentStart | — | — | SubagentStart | SubagentStart | SubagentStart | — | — | — | — | — |
| SubagentStop | SubagentStop | SubagentStop | — | subagentStop | — | — | SubagentStop | SubagentStop | SubagentStop | — | — | — | SubagentStop | — |
| PreCompact | PreCompact | PreCompact | PreCompress | preCompact | PreCompact | — | PreCompact | PreCompact | PreCompact | — | — | — | PreCompact | — |
| Notification | Notification | — | Notification | — | — | — | Notification | — | Notification | — | — | — | Notification | Notification |

Legend: `—` = the mapper writes nothing here — usually no analog, though a few (gemini's `Stop` via
`AfterAgent`, and its subagent events) are usable analogs it doesn't wire yet. `⚠️` = the event fires, but a CC tool-name matcher never
matches the harness's own tool names (gemini, antigravity-cli), so a matcher-scoped hook may not
trigger. `ᵈ` = the matcher is dropped entirely (cline, vscode-copilot), so the hook fires for
**every** tool, not just the matched one.

`copilot-cli` is not in the map: it copies the whole `hooks/hooks.json` verbatim, CC event names
included, rather than going through the event mapper.

## Gotchas

- **Portability skip.** A hook whose `command` carries `${CLAUDE_PLUGIN_ROOT}` is dropped on every
  config-merge backend. Point hooks at your own binary by name (`mytool self-heal`), which is what the
  `hooks-everywhere` example ships. See [Plugin tree](Plugin-Tree#claude_plugin_root-portability).
- **`${AGENTGEAR_CLIENT}` expands instead of skipping.** Each backend substitutes it for its own
  client id in the hook `command`, so one hook can tell which harness invoked it. See
  [Plugin tree](Plugin-Tree#agentgear_client-per-harness-client-id).
- **codex writes but stays inert** until the user trusts the hook in codex's `/hooks` TUI (a
  content-hash gate) — not a bug. kimi has no such gate; its hooks fire as soon as written.
- **Matcher drift (`⚠️`).** gemini's `BeforeTool`/`AfterTool` and the antigravity-cli tool events
  take a CC tool-name matcher that never matches the harness's own tool names. cline and
  vscode-copilot drop the `matcher` entirely.
- **antigravity-cli** accepts only five event names (`PreToolUse`, `PostToolUse`, `PreInvocation`,
  `PostInvocation`, `Stop`), so `SessionStart` is skipped and `UserPromptSubmit` lands on
  `PreInvocation`.
- **cline** hooks are event-named executables in a dir the CLI scans — `~/Documents/Cline/Hooks/`
  (user) and `.clinerules/hooks/` (project), a sibling of `Rules`, not a child.
- **kiro** hosts hooks only inside user-owned per-agent config files; its run-default agent is a
  setting, not a file, so there is no target agentgear can own without editing the user's agents.

## See also

- [Capabilities](Capabilities) — the index and the portability filter.
- [Scopes & gates](Capability-Scopes) — the trust gates in full.
- [Agent backends](Agent-Backends) — the shared hook renderer and adding a backend.
