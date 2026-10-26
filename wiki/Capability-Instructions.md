# Instructions

Instructions are a host's always-loaded guidance text, supplied through `PluginHost::instructions`
(the derive exposes it as `instructions_fn`). Like the [status line](Capability-Status-Line) and
unlike the five tree surfaces, this one does not ride a file in the plugin tree — it is a string
the host returns at runtime, delivered through each harness's native context channel.

**20 backends deliver it today.**

| harness | how it arrives |
|---|---|
| claude | through the MCP `initialize.instructions` field — no separate file |
| opencode | a dedicated `<plugin>-instructions.md`, registered in `opencode.json`'s `instructions[]` array |
| zed, codex, kilo, kimi, devin, droid, amp, omp, openclaw | a tagged block inside the harness's own global `AGENTS.md` (user scope: `~/.config/zed/AGENTS.md`, `$CODEX_HOME/AGENTS.md`, `~/.config/kilo/AGENTS.md`, `~/.kimi-code/AGENTS.md`, `~/.config/devin/AGENTS.md`, `~/.factory/AGENTS.md`, `$XDG_CONFIG_HOME/amp/AGENTS.md`, `~/.omp/agent/AGENTS.md`, `~/.openclaw/workspace/AGENTS.md`), delimited by `<!-- agentgear:<plugin-id> begin/end -->` markers so the user's own lines and a second plugin's block survive install and uninstall byte-identical |
| gemini, qwen-code, crush, goose, copilot-cli | the same tagged block inside each harness's own always-loaded context file (user scope): `GEMINI.md` (gemini), `QWEN.md` (qwen-code), `CRUSH.md` beside `crush.json` (crush — the first default of crush's `global_context_paths`, which tracks the `CRUSH_GLOBAL_CONFIG` override together with every other path this backend writes), `.goosehints` (goose), `copilot-instructions.md` (copilot-cli, `$COPILOT_HOME` — a file VS Code and JetBrains Copilot also read, accepted limit) |
| cline, augment, kiro | the same tagged block inside a whole plugin-owned file in a scanned rules dir (user scope): cline `<CLINE_DIR\|~/.cline>/rules/agentgear-<plugin>.md`, augment `~/.augment/rules/agentgear-<plugin>.md`, kiro `~/.kiro/steering/agentgear-<plugin>.md` |
| cursor | one `alwaysApply: true` `.mdc` rule per scope in `.cursor/rules/`, owned by its `x-agentgear` frontmatter tag so a user-authored file at the same name is skipped whole, never clobbered or swept |

Five backends stay unwritten. Four of those are settled probe verdicts, not open work: antigravity (the one doc-named channel, `~/.gemini/config/rules/` with `trigger: always_on`, never loads at the observable tier and its load leg is auth-gated — an authenticated IDE session opening a seeded always-on rule would reopen it), antigravity-cli (the channel `~/.gemini/config/rules/<file>.md` is probe-backed, but html-comment survival into agy's assembled prompt is auth-gated — an authenticated session showing the block arrive would reopen it), vscode-copilot and jetbrains-copilot (every always-loaded user-scope file is shared with the Copilot CLI or Claude Code — they read copilot-cli's `copilot-instructions.md` block; a harness-owned channel would reopen them). `pi` writes nothing at all. Because it sits outside the five-surface components IR (which has no field for it), a new backend cannot pick it up by rendering the IR — it needs explicit wiring like opencode's or the shared tagged-block renderer.

## See also

- [Capabilities](Capabilities) — the index.
- [Getting started](Getting-Started) — the `instructions_fn` derive attribute.
- [Agent backends](Agent-Backends) — the customization ceiling.
