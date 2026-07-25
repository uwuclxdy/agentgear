# Capabilities

A plugin ships five kinds of thing in its tree — MCP servers, hooks, commands, agents, skills —
and its host declares two more at runtime: instructions and a status line. A harness hosts some
subset of those, in its own config shape. This page is the index: what each capability is, how
many harnesses host it, and where the per-harness detail lives. Start here, then open the
capability you care about.

For the mirror axis — one harness at a time, everything it does — see
[Harness comparison](Harness-Comparison).

## Two install models

How a capability reaches a harness depends on which of two models the backend uses.

- **Plugin-native** (`claude`, `copilot-cli`). agentgear materializes the plugin tree and hands it
  to the tool's own plugin CLI (`claude plugin` / `copilot plugin`). The tool copies the whole tree
  and reads every surface itself, so there is no per-capability translation — every capability is
  served natively or not at all by that tool.
- **Config-merge** (the other 23 backends). No tool here has a plugin concept, so each backend
  read-modify-writes the tool's own config file, rendering each capability into that tool's native
  shape. A backend writes a capability only when the tool has a surface for it; a missing surface is
  skipped, never guessed. See [How it works](How-It-Works) and
  [Agent backends](Agent-Backends) for the mechanics.

## Capability index

The plugin surfaces, most-to-least widely hosted. "Hosts" counts backends that land the capability
(plugin-native + config-merge translated).

| capability | what the plugin ships | hosts | detail |
|---|---|---|---|
| MCP servers | `mcpServers` in `plugin.json` / `.mcp.json` | 24 of 25 (all but `pi`) | [MCP](Capability-MCP) |
| Hooks | `hooks/hooks.json` | 15 | [Hooks](Capability-Hooks) |
| Commands | `commands/**/*.md` | 14 | [Commands](Capability-Commands) |
| Agents | `agents/**/*.md` | 13 | [Agents](Capability-Agents) |
| Skills | `skills/<name>/SKILL.md` | 13 | [Skills](Capability-Skills) |
| Instructions | `PluginHost::instructions` (not a tree file) | 2 | [Instructions](Capability-Instructions) |
| Status line | `PluginHost::statusline` (not a tree file) | 5 | [Status line](Capability-Status-Line) |

Counts move as coverage lands; the linked page carries the exact per-harness list and the reason
behind every skip.

## Cross-cutting axes

Not surfaces a plugin ships, but how a backend decides *whether* and *where* to write.

| axis | the question it answers | detail |
|---|---|---|
| Detection & config roots | is the tool installed, and where does its config live (incl. env overrides)? | [Detection](Capability-Detection) |
| Scopes & gates | user vs project, and what trust gate stands between a write and it taking effect | [Scopes](Capability-Scopes) |
| Native CC-tree ingestion | which tools read the plugin tree directly, so a translation is redundant | [Native ingestion](Capability-Native-Ingestion) |

## What every backend guarantees

Uniform across all 23 config-merge backends, regardless of capability:

- **Detect-gated.** Nothing is written for a tool that is not on the machine. `self_heal` adopts it
  once the tool appears.
- **Never clobber.** Every write is read-modify-write; only the entries agentgear owns (keyed by the
  plugin's server/hook/command names) are touched. A config it cannot parse is refused, never
  overwritten.
- **Idempotent.** A second reconcile with no drift writes zero bytes and reports `NoOp`.
- **Symmetric remove.** `remove` deletes exactly what `reconcile` wrote and leaves the rest,
  including a retired-path sweep that only deletes tag-owned files. That covers shape as well as
  keys: a container our own keys emptied is taken back, and a config file left holding nothing goes
  with it. A container you were already keeping empty is measured as yours and survives.
- **Never resurrect / re-enable.** A user who deletes or disables an entry keeps it that way; only an
  explicit `install`/`update` re-enables.

The [status line](Capability-Status-Line) is the one surface these cannot cover as written: its
slot holds a single value, so there is nothing to merge beside. It keeps the same promise a
different way — stash the value it displaces, restore it on uninstall.

## The `${CLAUDE_PLUGIN_ROOT}` portability filter

`${CLAUDE_PLUGIN_ROOT}` expands only inside Claude Code's plugin runtime. Nothing expands it in a
config file agentgear merges into, so every config-merge backend **skips** an MCP server whose
`command` or `args` carry the token, and any hook whose `command` does, rather than write a literal
path that would fail to launch. It affects the [MCP](Capability-MCP) and [Hooks](Capability-Hooks)
surfaces; the plugin-native backends never run it. Full rule and the "installs clean, does nothing"
symptom: [Plugin tree](Plugin-Tree#claude_plugin_root-portability).

## The `${AGENTGEAR_CLIENT}` client-id token

`${AGENTGEAR_CLIENT}` is the portability filter's counterpart: agentgear knows this token too, and
**expands** it to each backend's own id (`claude`, `codex`, `copilot-cli`, …) in a hook `command`
and an MCP server's `command`/`args`, rather than skipping it. Write it once in your plugin tree
and every harness renders its own id. Full rule:
[Plugin tree](Plugin-Tree#agentgear_client-per-harness-client-id).

## See also

- [Harness comparison](Harness-Comparison) — the harness-first at-a-glance grid.
- [Plugin tree](Plugin-Tree) — what the tree holds and what is parsed from where.
- [Agent backends](Agent-Backends) — the `AgentBackend` trait, install models, adding your own.
