# Instructions

Instructions are a host's always-loaded guidance text, supplied through `PluginHost::instructions`
(the derive exposes it as `instructions_fn`). Like the [status line](Capability-Status-Line) and
unlike the five tree surfaces, this one does not ride a file in the plugin tree — it is a string
the host returns at runtime, delivered through each harness's native context channel.

**6 backends deliver it today.**

| harness | how it arrives |
|---|---|
| claude | through the MCP `initialize.instructions` field — no separate file |
| opencode | a dedicated `<plugin>-instructions.md`, registered in `opencode.json`'s `instructions[]` array |
| zed, codex, kilo, kimi | a tagged block inside the harness's own global `AGENTS.md` (user scope), delimited by `<!-- agentgear:<plugin-id> begin/end -->` markers so the user's own lines and a second plugin's block survive install and uninstall byte-identical |

Every other backend has no always-loaded instruction channel agentgear targets, so the text is
simply not delivered there. Because it sits outside the five-surface components IR (which has no
field for it), a new backend cannot pick it up by rendering the IR — it needs explicit wiring like
opencode's or the shared tagged-block renderer the `AGENTS.md` family uses.

## See also

- [Capabilities](Capabilities) — the index.
- [Getting started](Getting-Started) — the `instructions_fn` derive attribute.
- [Agent backends](Agent-Backends) — the customization ceiling.
