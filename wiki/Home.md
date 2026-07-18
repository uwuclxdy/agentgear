# agentgear wiki

A Rust library and derive macro for shipping a coding-agent plugin from a binary. It gives the binary a canonical install/uninstall/update/self-heal lifecycle for 25 agents. Claude Code and copilot-cli get the full plugin flow: a `setup` subcommand replaces the user typing `/plugin marketplace add` + `/plugin install` (or `copilot plugin marketplace add` + `plugin install` on copilot's own CLI). The other 23 config-merge backends merge into their own config files instead.

The [README](https://github.com/uwuclxdy/agentgear#readme) is the overview. These pages hold the reference.

## Mental model

The crate lives in your binary, not in the plugin tree. The binary embeds the plugin at compile time as a compressed blob and hands it to one or more agent backends at runtime. 25 backends ship today: Claude Code, copilot-cli, and 23 config-merge agents; the full list is on the [Agent Backends](Agent-Backends) page.

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
| [Getting Started](Getting-Started) | add the crate, the derive, the build guard, hook wiring |
| [How It Works](How-It-Works) | lifecycle to CLI mapping, materialize, the self-heal state table |
| [Agent Backends](Agent-Backends) | the unsealed `AgentBackend` trait, install models, env overrides, hook renames |
| [Harness Comparison](Harness-Comparison) | side-by-side: support, config paths, remote fidelity, native CC-config interop |
| [Doctor](Doctor) | the health report: shared checks, per-agent checks, fix hints |

## Status

v1 ships 25 agent backends: Claude Code + copilot-cli (full plugin lifecycle) plus 23 config-merge backends. Every non-CC backend was verified against its real shipping binary on 2026-07-16 (copilot-cli's native rewrite 2026-07-18), with positive and negative controls before any parse was trusted. 19 per-tool docker legs (the GUI/IDE and no-surface backends have none) pass locally; the CI push is pending. Hermetic and unit tests are green in plain `cargo test`. The `AgentBackend` trait is unsealed. Linux is CI-gated, macOS is tested, Windows is designed in but not gated in CI.
