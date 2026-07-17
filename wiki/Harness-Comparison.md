# Harness comparison

Side-by-side reference for the 25 backends agentgear ships: Claude Code plus 24 config-merge
agents. Every cell was checked against the real shipping binary (or the tool's shipped extension
source, for the IDE-bound ones) on 2026-07-16, with positive and negative controls before any
parse was trusted.

"Translated" means agentgear read-modify-writes the tool's own config file so the plugin's
components land in that tool's native shape. Claude Code is the exception: it gets the full plugin
lifecycle through `claude plugin`, no config merge. A backend writes only when it detects the tool
installed and the tool has a surface at the target scope.

Where a row says **known limitation**, the tool never reads what agentgear currently writes, or
reads it in the wrong shape. Those are real user-visible gaps today, with fixes queued. They are
called out rather than hidden.

## Support overview

What each backend translates. `—` means the surface is skipped, with the reason. Skills have no
backend on any tool yet.

| harness | mcp | hooks | commands | agents |
|---|---|---|---|---|
| claude | ✓ | ✓ | ✓ | ✓ |
| codex | ✓ | ✓ (inert until trusted in `/hooks`) | ✓ | ✓ |
| cursor | ✓ | ✓ | ✓ | ✓ |
| devin | ✓ | ✓ | ✓ | ✓ |
| qwen-code | ✓ | ✓ | ✓ | ✓ |
| droid | ✓ | ✓ | ✓ | ✓ |
| augment | ✓ | ✓ | ✓ | ✓ |
| gemini | ✓ | ✓ | ✓ | — (agent surface not translated) |
| cline | ✓ | ✓ (project only; user dir wrong, known limitation) | ✓ (workflows) | — (surface unimplemented) |
| copilot-cli | ✓ | ✓ | — (only surface is project-scoped) | ✓ |
| vscode-copilot | ✓ | ✓ | — (out of scope) | ✓ |
| kimi | ✓ | ✓ | — (slash-command analog is a skill) | — (built-in sub-agents only) |
| kiro | ✓ | — (kiro hosts hooks only inside user-owned agent configs; no target agentgear can own) | — | — |
| antigravity-cli | ✓ | ✓ (user-scope path + events wrong, known limitation) | — | — |
| goose | ✓ | ✓ | — | — |
| crush | ✓ | ✓ | — (TUI-only, not loaded by `crush run`) | — |
| opencode | ✓ | — (JS/TS plugin API only) | ✓ | ✓ |
| omp | ✓ | — (TS extension API only) | ✓ | ✓ |
| kilo | ✓ | — (no hooks key in the schema) | ✓ | ✓ |
| jetbrains-copilot | ✓ | — | — | — |
| zed | ✓ | — | — | — |
| openclaw | ✓ | — (JS/TS module hooks only) | — | — |
| antigravity | ✓ | — | — | — |
| amp | ✓ | — | — | — |
| pi | — (no native mcp surface) | — | — | — |

## Config locations

The user-scope config each backend writes. Most honor a home/config-dir env override; the notable
ones are in [Agent backends](Agent-Backends). Project scope, where a backend serves it, sits under
the working tree.

| harness | user config file |
|---|---|
| claude | `<config>/plugins/` (via `claude plugin`; `CLAUDE_CONFIG_DIR`) |
| amp | `~/.config/amp/settings.json` |
| antigravity | `~/.gemini/config/mcp_config.json` |
| antigravity-cli | `~/.gemini/config/mcp_config.json` |
| augment | `~/.augment/settings.json` |
| cline | `~/.cline/data/settings/cline_mcp_settings.json` (mcp); `~/Documents/Cline/` (hooks, workflows) |
| codex | `~/.codex/config.toml` |
| copilot-cli | `~/.copilot/mcp-config.json` |
| crush | `~/.config/crush/crush.json` |
| cursor | `~/.cursor/mcp.json` |
| devin | `~/.config/devin/config.json` |
| droid | `~/.factory/` (`mcp.json`, `hooks.json`, `commands/`, `droids/`) |
| gemini | `~/.gemini/settings.json` |
| goose | `~/.config/goose/config.yaml` |
| jetbrains-copilot | `<config>/github-copilot/intellij/mcp.json` |
| kilo | `~/.config/kilo/kilo.json` |
| kimi | `~/.kimi-code/mcp.json` + `config.toml` |
| kiro | `~/.kiro/settings/mcp.json` |
| omp | `~/.omp/agent/mcp.json` |
| openclaw | `~/.openclaw/openclaw.json` |
| opencode | `~/.config/opencode/opencode.json` |
| pi | none (detect-only) |
| qwen-code | `~/.qwen/settings.json` |
| vscode-copilot | project only: `<repo>/.vscode/mcp.json` + `.github/` |
| zed | `~/.config/zed/settings.json` |

## Remote MCP fidelity

stdio is the verified path on every backend. Remote (http/sse) servers stay best-effort: some tools
read the rendered `{type, url, headers}` shape as-is, some key the transport off a different field,
some reject the shape and void the file. Per-tool fixes are queued.

| harness | remote verdict |
|---|---|
| claude | native, no translation (Claude Code reads the tree itself) |
| amp | faithful. `type` is inert; the tool infers transport from `url` |
| antigravity | faithful: sse rendered as native `{serverUrl}`; http skipped (no landing) |
| antigravity-cli | faithful: sse rendered as native `{serverUrl}`; http skipped (no landing) |
| augment | faithful. byte-matches the tool's own writer |
| cline | faithful: rendered with cline's literal `streamableHttp`/`sse` transport values |
| codex | faithful. streamable-HTTP only; sse maps onto the same `url` |
| copilot-cli | faithful. `tools:["*"]` is required (a bare string voids the file) |
| crush | faithful. both transports dial-proven |
| cursor | faithful. `type` genuinely picks the transport |
| devin | faithful: rendered with devin's native `{url, transport}` form |
| droid | faithful. `type` is the live discriminator |
| gemini | faithful. native `{url, type, headers?}` |
| goose | http faithful; sse skipped (goose runtime-refuses sse, so a dead extension is never written) |
| jetbrains-copilot | faithful: `{type, url}` (headers will nest under `requestInit.headers` once plugins carry them) |
| kilo | faithful. `type` mandatory, dial-proven |
| kimi | faithful: rendered with kimi's native `{url, transport}` form |
| kiro | unproven (login-walled). native `{url, headers}`, examples carry no `type` |
| omp | faithful. matches native exactly |
| openclaw | faithful, but the tool's own fixer rewrites the transport key → probe churn |
| opencode | faithful. both transports collapse into `remote` |
| pi | skipped. core pi has no native mcp surface |
| qwen-code | faithful: rendered with qwen's key-presence form (http `{httpUrl}`, sse `{url}`) |
| vscode-copilot | http faithful (byte-preserved); sse skipped (VS Code rewrites sse to http, so writing it would churn) |
| zed | http faithful (native `{url, headers}`); sse skipped (zed has a single remote transport) |

## Native Claude-Code-config interop

Several tools read Claude Code's own config or plugin trees directly, the exact tree agentgear's
materializer already produces. Where a tool ingests the full plugin tree natively, translating its
config is redundant, so agentgear retires the overlapping translation where the native path already
covers it (omp's agents, once its provider is on and CC's registry lists the plugin). Scope of
ingestion is the honest limit per tool.

| harness | reads | scope |
|---|---|---|
| codex | `codex plugin marketplace add`/`add` parse `.claude-plugin/marketplace.json` + `plugin.json` | full tree. mcp live-merged at read, hooks copied but never fire |
| copilot-cli | `copilot plugin marketplace add`/`install` read `.claude-plugin/plugin.json` + `marketplace.json` 1:1 | full tree → `~/.copilot/installed-plugins/`. `mcp list` shows `Source: Plugin`. Separately reads project `.claude/agents`, `.claude/skills`, `.claude/settings.json` |
| cursor | aggregator reads `~/.claude/plugins/installed_plugins.json`, `~/.claude/settings.json` `enabledPlugins`, then loads `.claude-plugin/plugin.json` per install | full tree with `${CLAUDE_PLUGIN_ROOT}` substitution (source-proven, undocumented) |
| droid | `droid plugin marketplace add` reads `.claude-plugin/marketplace.json` + `plugin.json` | full tree → `~/.factory/plugins/` (binary-proven end to end) |
| qwen-code | `qwen extensions install` converts a `.claude-plugin/plugin.json` dir into a qwen extension | full tree, install-time and manual. carries skills through too |
| omp | first-class `claude-plugins` provider reads `~/.claude/plugins/installed_plugins.json` | agents only. agentgear retires its agent translation when omp's `claude-plugins` provider is on and CC's registry lists the plugin, so no double-register |
| openclaw | `plugins.load.paths` append, or drop the tree at `~/.openclaw/extensions/<id>/` (auto-detect) | full tree, "Claude-compatible bundles". hooks still register nothing |
| jetbrains-copilot | bundled agent's marketplace service reads `.claude-plugin/plugin.json`, `hooks/hooks.json`, `${CLAUDE_PLUGIN_ROOT}` | full tree per the format descriptor (source-proven, no IDE launched) |
| antigravity-cli | `agy plugin import <path>` copies the source dir verbatim | full tree, path-only. skills + agents ingest; hooks copied raw and never fire; `mcpServers` dropped |
| augment | `auggie plugin marketplace add` / `--plugin-dir` read `.augment-plugin/` or `.claude-plugin/` | full tree (vendor-documented; not exercised past the auth wall) |
| vscode-copilot | VS Code core discovers `.claude-plugin/marketplace.json` + `plugin.json`, expands `${CLAUDE_PLUGIN_ROOT}` | full tree, plus default-on loose `.claude/` hooks, agents, skills |
| devin | `read_config_from.claude` live-merges `~/.claude.json`, `.claude/settings.json`, `~/.claude/skills`, `~/.claude/agents`, `CLAUDE.md` | loose config only, not plugin bundles |
| amp | reads `.claude/skills`, `~/.claude/skills`, and CC's plugin-cache skills | skills only |
| crush | reads `.claude/skills` + `~/.claude/skills` as skill dirs | skills only |

Clean negatives (no CC-tree ingestion): antigravity (the IDE, distinct from `agy`), cline, gemini,
goose, kilo, kimi, kiro, opencode, pi, zed.

## See also

- [Agent backends](Agent-Backends): per-backend detail, env overrides, hook event renames, adding
  your own backend.
- [How it works](How-It-Works): the Claude Code lifecycle, materialize, self-heal.
