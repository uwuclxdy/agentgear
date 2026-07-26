# Harness comparison

Side-by-side reference for the 25 backends agentgear ships: Claude Code, copilot-cli, and 23
config-merge agents. Every cell was checked against the real shipping binary (or the tool's
shipped extension source, for the IDE-bound ones) on 2026-07-16 (copilot-cli's native rewrite
2026-07-18), with positive and negative controls before any parse was trusted.

"Translated" means agentgear read-modify-writes the tool's own config file so the plugin's
components land in that tool's native shape. Claude Code and copilot-cli are the exception: both
get the full plugin lifecycle through their own CLI (`claude plugin` / `copilot plugin`), no
config merge. A backend writes only when it detects the tool installed and the tool has a surface
at the target scope.

## Support overview

What each backend translates. `—` means the surface is skipped, with the reason. `claude` and
`copilot-cli` are plugin-native (the whole tree ships through the tool's own plugin CLI, not a
per-surface translation); every column reads native for both.

| harness | mcp | hooks | commands | agents | skills |
|---|---|---|---|---|---|
| claude | native | native | native | native | native |
| copilot-cli | native | native | native | native | native |
| cursor | ✓ | ✓ | ✓ | ✓ | ✓ |
| devin | ✓ | ✓ | ✓ | ✓ | ✓ |
| qwen-code | ✓ | ✓ | ✓ | ✓ | ✓ |
| droid | ✓ | ✓ | ✓ | ✓ | ✓ |
| codex | ✓ | ✓ (inert until trusted in `/hooks`) | ✓ | ✓ | — (not translated) |
| gemini | ✓ | ✓ (a CC tool-name matcher never matches gemini's own) | ✓ | ✓ | — (not translated) |
| augment | ✓ | ✓ | ✓ | ✓ | — (not translated) |
| crush | ✓ | ✓ | ✓ (TUI-only, not loaded by `crush run`) | — (no file-writable surface, issue #1807) | ✓ |
| kilo | ✓ | — (no hooks key in the schema) | ✓ | ✓ | ✓ |
| vscode-copilot | ✓ | ✓ (CC matcher dropped) | — (out of scope) | ✓ | — (out of scope) |
| cline | ✓ | ✓ (CC matcher dropped) | ✓ (workflows) | — (surface unimplemented) | — (surface unimplemented) |
| opencode | ✓ | — (JS/TS plugin API only) | ✓ | ✓ | — (not translated) |
| omp | ✓ | — (TS extension API only) | ✓ | ✓ | — (not translated) |
| kimi | ✓ | ✓ | — (slash-command analog is a skill) | — (built-in sub-agents only) | ✓ |
| goose | ✓ | ✓ | — | — | ✓ |
| kiro | ✓ | — (kiro hosts hooks only inside user-owned agent configs; no target agentgear can own) | — | — | ✓ |
| antigravity-cli | ✓ | ✓ (a CC tool-name matcher never matches `agy`'s own) | — | — | — |
| zed | ✓ | — | — | — | ✓ |
| openclaw | ✓ | — (JS/TS module hooks only) | — | — | ✓ |
| jetbrains-copilot | ✓ | — | — | — | — |
| antigravity | ✓ | — | — | — | — |
| amp | ✓ | — | — | — | — |
| pi | — (no native mcp surface) | — | — | — | — |

Every `✓` above carries one filter: `${CLAUDE_PLUGIN_ROOT}` expands only inside Claude Code's own
runtime, so a config-merge backend skips any mcp server whose **command or args** carry the token,
and any hook whose command does, rather than write an entry that would launch the literal path. The
canonical plugin-shipped server puts the token in `args`
(`{"command": "node", "args": ["${CLAUDE_PLUGIN_ROOT}/server.js"]}`), so that shape is the one that
gets dropped everywhere except Claude Code. A `✓` says the surface translates; it does not promise
every entry in it survives. `claude` and `copilot-cli` never run this filter, since both copy the
tree verbatim: Claude Code expands the token in its own runtime, and whether copilot's raw-shape
copy of `hooks/hooks.json` fires at all is unconfirmed.

Two more surfaces sit outside these five, both declared by the host rather than shipped in the
tree, so neither rides the components IR:

- **instructions** (`PluginHost::instructions`, always-loaded guidance text). Only `opencode`
  writes it today, a dedicated file registered in its `instructions[]` array; `claude` receives
  the same text through the MCP `initialize.instructions` field instead.
- **status line** (`PluginHost::statusline`). Five backends write it into their tool's own single
  slot: `claude`, `copilot-cli` (user scope only), `qwen-code`, `antigravity-cli` (user scope only),
  `droid`. That slot holds one value, so agentgear stashes whatever it displaces and restores it on
  uninstall. Full rule: [Status line](Capability-Status-Line).

## Config locations

The user-scope config each backend writes. A row names the file (or dir) carrying mcp, and a second
path whenever that backend's hooks, commands, agents, or skills land under a different root. Most
backends honor a home/config-dir env override; the notable ones are in
[Agent backends](Agent-Backends). Project scope, where a backend serves it, sits under the working
tree.

| harness | user config file |
|---|---|
| claude | `<config>/plugins/` (via `claude plugin`; `CLAUDE_CONFIG_DIR`); `<config>/settings.json` for a declared status line |
| copilot-cli | `~/.copilot/installed-plugins/<mkt>/<plugin>/` (via `copilot plugin`; native, whole tree copied; `COPILOT_HOME`); `~/.copilot/settings.json` for a declared status line |
| amp | `~/.config/amp/settings.json` |
| antigravity | `~/.gemini/config/mcp_config.json` |
| antigravity-cli | `~/.gemini/config/` (`mcp_config.json`, `hooks.json`); `~/.gemini/antigravity-cli/settings.json` for a declared status line |
| augment | `~/.augment/settings.json` |
| cline | `~/.cline/data/settings/cline_mcp_settings.json` (mcp); `~/Documents/Cline/` (hooks, workflows) |
| codex | `~/.codex/config.toml` |
| crush | `~/.config/crush/crush.json` |
| cursor | `~/.cursor/mcp.json` |
| devin | `~/.config/devin/config.json` |
| droid | `~/.factory/` (`mcp.json`, `hooks.json`, `commands/`, `droids/`, `settings.json` for a declared status line) |
| gemini | `~/.gemini/settings.json` |
| goose | `~/.config/goose/config.yaml` (mcp); `~/.agents/plugins/<plugin>/` (hooks, skills) |
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
| zed | `~/.config/zed/settings.json` (mcp); `~/.agents/skills/` (skills) |

## Scopes

Which scopes each backend serves. `Scope::User` targets the home-level config,
`Scope::Project { path }` that directory's. A backend with no surface at the requested scope is
skipped: no write, no marker, no error, so a project-scope install lands only on the backends
marked ✓ under project. Self-heal runs at user scope only.

`vscode-copilot` is the one project-only backend. Its user config is rejected on purpose: a
non-default VS Code profile relocates `User/mcp.json`; the active profile is VS Code internal state
that cannot be read off disk.

| harness | user | project | notes |
|---|---|---|---|
| claude | ✓ | ✓ | the project path reaches the CLI as its working directory |
| copilot-cli | ✓ | — | the `copilot` CLI takes no `--scope`; a native install is user-level. project `.mcp.json` is real, unwritten |
| amp | ✓ | — | `.amp/` needs `amp mcp approve <name>` and is scanned at the exact cwd, no walk-up |
| antigravity | ✓ | — | |
| antigravity-cli | ✓ | ✓ | project scope loads only after the folder is trusted |
| augment | ✓ | — | the tool serves `.augment/` too; the backend stays user-only |
| cline | ✓ | ✓ | mcp is user-global whichever scope you pass; hooks and workflows honor both |
| codex | ✓ | ✓ | project needs `trust_level="trusted"` in the user `config.toml`. a trusted session loads the project's servers; `codex mcp list` never shows them |
| crush | ✓ | ✓ | a same-key entry in `$XDG_DATA_HOME/crush/crush.json` silently outranks ours |
| cursor | ✓ | ✓ | project wins a same-key mcp collision |
| devin | ✓ | ✓ | project overrides user on a same name |
| droid | ✓ | ✓ | droid reads a third tier between them, any ancestor's `.factory/mcp.json`, outside our write surface |
| gemini | ✓ | ✓ | an untrusted folder suppresses mcp and commands at both scopes together; project hooks are blocked, user hooks are not |
| goose | ✓ | — | no project extensions file |
| jetbrains-copilot | ✓ | — | `<project>/.github/mcp.json` is real, unwritten |
| kilo | ✓ | ✓ | |
| kimi | ✓ | — | `<cwd>/.kimi-code` is a real native mcp scope, undeclared because hooks have no project surface |
| kiro | ✓ | ✓ | |
| omp | ✓ | ✓ | `OMP_PROFILE`/`PI_PROFILE` relocate user scope to `~/.omp/profiles/<name>/agent`, which the backend does not track |
| openclaw | ✓ | — | one user-global file regardless of cwd |
| opencode | ✓ | ✓ | |
| pi | ✓ | — | detect-only, nothing written at either scope |
| qwen-code | ✓ | ✓ | project hooks need folder trust; project mcp, commands and agents apply unconditionally |
| vscode-copilot | — | ✓ | |
| zed | ✓ | ✓ | the project write is unconditional; `trust_all_worktrees` gates only whether zed auto-starts project mcp servers |

## Remote MCP fidelity

stdio is the verified path on every backend; remote (http/sse) fidelity varies per tool. The full
per-harness verdict table now lives on the capability page:
[MCP § remote fidelity](Capability-MCP#remote-httpsse-fidelity).

## Native Claude-Code-config interop

Several tools read Claude Code's own config or plugin trees directly — the exact tree agentgear
materializes — so a translation is redundant and agentgear retires it. The full who-reads-what table
and the retirement rules moved to [Native ingestion](Capability-Native-Ingestion).

## See also

- [Capabilities](Capabilities): the capability-first view — one page per surface, with the deep
  per-harness tables (MCP shapes + fidelity, hook events, native ingestion).
- [Agent backends](Agent-Backends): the trait mechanics and adding your own backend.
- [How it works](How-It-Works): the Claude Code lifecycle, materialize, self-heal.
