# agentgear wiki

A Rust library and derive macro for shipping a coding-agent plugin from a binary. It gives the binary a canonical install/uninstall/update/self-heal lifecycle for the Claude Code plugin it ships, so a `setup` subcommand replaces the user typing `/plugin marketplace add` + `/plugin install`.

The [README](https://github.com/uwuclxdy/agentgear#readme) is the overview. These pages hold the reference.

## Mental model

The crate lives in your **binary**, not in the plugin tree. The binary embeds the plugin at compile time as a compressed blob and hands it to one or more agent backends at runtime. Today the only backend is Claude Code.

```text
your binary  ──derive──▶  PluginHost   ──reconcile──▶  AgentBackend (claude)  ──▶  claude plugin CLI
   │                                                                                      │
   └── embeds plugin/ (.tar.br blob) ── materialize ──▶ ~/.local/share/<name>/current ────┘
```

## Pages

| page | topic |
|---|---|
| [Getting Started](Getting-Started) | add the crate, the derive, the build guard, hook wiring |
| [How It Works](How-It-Works) | lifecycle to CLI mapping, materialize, the self-heal state table |
| [Agent Backends](Agent-Backends) | the sealed `AgentBackend` trait and how a second agent plugs in |
| [Doctor](Doctor) | the six health checks and their fix hints |

## Status

v1 implements the Claude Code backend. The `AgentBackend` trait is a sealed seam for other coding agents and stays sealed until a second backend lands from real use. Linux and macOS are tested; Windows support is designed in but not gated in CI.
