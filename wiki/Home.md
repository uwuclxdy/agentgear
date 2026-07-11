# agentgear wiki

A Rust library and derive macro for shipping a coding-agent plugin from a binary. It gives the binary a canonical install/uninstall/update/self-heal lifecycle across seven agents. Claude Code gets the full plugin flow: a `setup` subcommand replaces the user typing `/plugin marketplace add` + `/plugin install`. codex, opencode, gemini, cursor, cline, and devin get config-merge into their own config files instead.

The [README](https://github.com/uwuclxdy/agentgear#readme) is the overview. These pages hold the reference.

## Mental model

The crate lives in your **binary**, not in the plugin tree. The binary embeds the plugin at compile time as a compressed blob and hands it to one or more agent backends at runtime. Seven backends ship today: Claude Code, codex, opencode, gemini, cursor, cline, and devin (Devin Local).

```text
your binary  ──derive──▶  PluginHost  ──reconcile──▶  AgentBackend
                                                        │
                                        claude ──▶ claude plugin CLI (marketplace + materialize)
                          codex/opencode/gemini/cursor/cline/devin ──▶ each tool's own config file

embeds plugin/ (.tar.br blob) ── materialize ──▶ ~/.local/share/<name>/current   (claude backend only)
```

## Pages

| page | topic |
|---|---|
| [Getting Started](Getting-Started) | add the crate, the derive, the build guard, hook wiring |
| [How It Works](How-It-Works) | lifecycle to CLI mapping, materialize, the self-heal state table |
| [Agent Backends](Agent-Backends) | the unsealed `AgentBackend` trait, the seven backends, and the two install models |
| [Doctor](Doctor) | the six health checks and their fix hints |

## Status

v1 ships seven agent backends: Claude Code (full plugin lifecycle) plus codex, opencode, gemini, cursor, cline, and devin (config-merge). The `AgentBackend` trait is unsealed. Linux and macOS are tested; Windows support is designed in but not gated in CI.
