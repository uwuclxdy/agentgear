# agentgear wiki

A Rust library and derive macro for shipping a coding-agent plugin from a binary. It gives the binary a canonical install/uninstall/update/self-heal lifecycle for 25 agents. Claude Code and copilot-cli get the full plugin flow: a `setup` subcommand replaces the user typing `/plugin marketplace add` + `/plugin install` (or `copilot plugin marketplace add` + `plugin install` on copilot's own CLI). The other 23 config-merge backends merge into their own config files instead.

The [README](https://github.com/uwuclxdy/agentgear#readme) is the overview. These pages hold the reference.

## Mental model

The crate lives in your binary, not in the plugin tree. The binary embeds the plugin at compile time as a compressed blob and hands it to one or more agent backends at runtime. 25 backends ship today: Claude Code, copilot-cli, and 23 config-merge agents; the full list is on the [Agent backends](Agent-Backends) page.

```text
your binary  ──derive──▶  PluginHost  ──reconcile──▶  AgentBackend
                                                        │
                     claude / copilot-cli ──▶ the tool's own plugin CLI (marketplace + materialize)
                     23 config-merge agents (codex, cursor, gemini, …) ──▶ each tool's own config file

embeds plugin/ (.tar.br blob) ── materialize ──▶ ~/.local/share/<name>/current   (claude + copilot-cli only)
```

## Pages

| page | topic |
|---|---|
| [Getting started](Getting-Started) | add the crate, the derive, the build guard, hook wiring |
| [Plugin tree](Plugin-Tree) | what `plugin/` holds, what each backend can translate out of it |
| [How it works](How-It-Works) | lifecycle to CLI mapping, materialize, the self-heal state table |
| [Agent backends](Agent-Backends) | the unsealed `AgentBackend` trait, install models, adding your own |
| [Harness comparison](Harness-Comparison) | harness-first at-a-glance: support grid, config paths, scopes |
| [Capabilities](Capabilities) | capability-first depth: a page each for MCP, hooks, commands, agents, skills, instructions + detection, scopes, native ingestion |
| [Testing your host](Testing-Your-Host) | hermetic runs against a temp `HOME`, reading back what each backend wrote |
| [Doctor](Doctor) | the health report: shared checks, per-agent checks, fix hints |
| [Types and errors](Types-and-Errors) | the programmatic API: `Outcome`, `AgentReport`, `Capabilities`, the components IR, `Error` |

## Status

v1 ships 25 agent backends: Claude Code + copilot-cli (full plugin lifecycle) plus 23 config-merge backends. Every non-CC backend was verified against its real shipping binary, with positive and negative controls before any parse was trusted. 19 per-tool docker legs (the GUI/IDE and no-surface backends have none) run in CI as a matrix, one job per tool. Hermetic and unit tests are green in plain `cargo test`. The `AgentBackend` trait is unsealed.

Linux is CI-gated. Windows was probed by hand against Claude Code 2.1.201: a junction created without elevation, with `validate`, `marketplace add`, `install`, and `list --json` all resolving through `current`; the no-BOM and no-trailing-whitespace path rules in `materialize` come from that same run. macOS is a first-class target in the design but is not exercised on a real host in CI. Every CI job runs on `ubuntu-latest`.
