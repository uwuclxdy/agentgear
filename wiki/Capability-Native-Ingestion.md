# Native CC-tree ingestion

Several tools read Claude Code's own config or plugin trees directly — the exact tree agentgear's
materializer already produces. Where a tool ingests the full plugin tree natively, translating its
config is redundant, so agentgear **retires the overlapping translation** where the native path
already covers it. This is the cross-cutting integration story: for some harnesses, the best
integration is to let the tool read the CC tree, not to write its config.

## Who reads the tree

| harness | reads | scope of ingestion |
|---|---|---|
| codex | `codex plugin marketplace add`/`add` parse `.claude-plugin/marketplace.json` + `plugin.json` | full tree. mcp live-merged at read, hooks copied but never fire |
| cursor | aggregator reads `~/.claude/plugins/installed_plugins.json`, then loads `.claude-plugin/plugin.json` per install | full tree with `${CLAUDE_PLUGIN_ROOT}` substitution (source-proven, undocumented) |
| droid | `droid plugin marketplace add` reads `.claude-plugin/marketplace.json` + `plugin.json` | full tree → `~/.factory/plugins/` (binary-proven end to end) |
| qwen-code | `qwen extensions install` converts a `.claude-plugin/plugin.json` dir into a qwen extension | full tree, install-time and manual; carries skills through |
| omp | first-class `claude-plugins` provider reads `~/.claude/plugins/installed_plugins.json` | agents only |
| openclaw | `plugins.load.paths` append, or drop the tree at `~/.openclaw/extensions/<id>/` (auto-detect) | full tree, "Claude-compatible bundles"; hooks still register nothing |
| jetbrains-copilot | bundled agent's marketplace service reads `.claude-plugin/plugin.json`, `hooks/hooks.json`, `${CLAUDE_PLUGIN_ROOT}` | full tree per the format descriptor (source-proven, no IDE launched) |
| antigravity-cli | `agy plugin import <path>` copies the source dir verbatim | full tree, path-only. skills + agents ingest; hooks copied raw and never fire; `mcpServers` dropped |
| augment | `auggie plugin marketplace add` / `--plugin-dir` read `.augment-plugin/` or `.claude-plugin/` | full tree (vendor-documented; not exercised past the auth wall) |
| vscode-copilot | VS Code core discovers `.claude-plugin/marketplace.json` + `plugin.json`, expands `${CLAUDE_PLUGIN_ROOT}` | full tree, plus default-on loose `.claude/` hooks, agents, skills |
| devin | `read_config_from.claude` live-merges `~/.claude.json`, `.claude/settings.json`, `~/.claude/skills`, `~/.claude/agents`, `CLAUDE.md` | loose config only, **not** plugin bundles |
| amp | reads `.claude/skills`, `~/.claude/skills`, and CC's plugin-cache skills (`~/.claude/plugins/cache/**/skills/*`) | **partial**: skills only, but a plugin's skills surface via the cache |

**Clean negatives** (no plugin-tree ingestion): antigravity (the IDE, distinct from `agy`), cline,
crush (reads loose `.claude/skills` as skill dirs only), gemini, goose, kilo, kimi, kiro, opencode,
pi, zed.

## Where translation retires

The native path is not just a curiosity — agentgear stands its translation down when the tool already
covers the plugin, to avoid double-registration:

- **copilot-cli** goes furthest: since 2026-07-18, native ingestion **is** the backend's whole
  mechanism, not a redundant path beside a translation.
- **omp's agent write retires** when Claude Code's registry lists the plugin *and* omp's
  `claude-plugins` provider is on (default). Double-registration then only bites if the provider is
  user-disabled or `CLAUDE_CONFIG_DIR` relocates the registry off the HOME path omp reads.
- **cursor's whole translation retires** when Claude Code's registry (HOME-based) lists the plugin.
  The gate does not check cursor's `enabledPlugins`, a deliberately conservative gap: a
  CC-side-disabled-but-installed plugin still reads as covered.

## Gotchas

- **Ingestion scope is the honest limit.** "full tree" for antigravity-cli still drops `mcpServers`
  and never fires the copied hooks; codex live-merges mcp but its copied hooks never register. Read
  the scope column, not just the ingests-or-not.
- **Loose config ≠ plugin bundle.** devin and crush read loose `.claude/` files rather than a
  plugin-tracked tree, so a skill inside a plugin never surfaces there. Only a loose
  `~/.claude/skills/<name>/SKILL.md` does. amp is the partial case: it reaches CC's plugin-cache
  skills, so a plugin's skills do surface (skills only, never mcp/hooks/commands).

## See also

- [Capabilities](Capabilities) — the index.
- [Agents](Capability-Agents) — where the agent translation retires.
- [How it works](How-It-Works) — the materialize pipeline that produces the tree tools read.
