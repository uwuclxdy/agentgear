# Commands

A command is a `commands/**/*.md` file — a reusable slash-command prompt. agentgear copies each into
the target tool's own command (or "prompt" / "workflow") directory, dropping or translating the CC
frontmatter to match what the tool's loader expects and prefixing the plugin name so two plugins
never collide.

**14 backends host commands.** The rest either have no file-writable command surface, or expose the
analog as a skill (kimi) rather than a flat command dir.

## Where each command lands

| harness | where written | notes |
|---|---|---|
| claude | `commands/**/*.md` (plugin tree, native) | nested paths kept, name is the file stem |
| copilot-cli | native, copied verbatim into `~/.copilot/installed-plugins/` | whether copilot's engine loads a plugin's `commands/` dir is unconfirmed |
| augment | `~/.augment/commands/<plugin>-<name>.md` | tool also reads `~/.claude/commands/` natively |
| codex | `prompts/<plugin>-<name>.md` | user scope only, flat (no subdirs scanned) |
| crush | `~/.config/crush/commands/<plugin>/*.md` (user) · `<project>/.crush/commands/<plugin>/*.md` (project) | body only (frontmatter dropped); **TUI palette only, never loaded by `crush run`** |
| cursor | `~/.cursor/commands/<plugin>-<stem>.md` | plain markdown, frontmatter stripped |
| devin | `skills/<plugin>-<stem>/SKILL.md` | CC commands land as devin **skills** |
| droid | `commands/<plugin>-<stem>.md` | flat top-level only (nested dirs ignored) |
| gemini | `~/.gemini/commands/<plugin>/*.toml` | recursive, `:` namespacing |
| kilo | `<base>/commands/<plugin>-<stem>.md` | kilo scans `{command,commands}/**/*.md` |
| cline | `<store>/Workflows/<plugin>-<name>.md` (user) · `.clinerules/workflows/<plugin>-<name>.md` (project) | CC commands land as cline **workflows**; frontmatter dropped |
| omp | `<base>/commands/<plugin>-<stem>.md` | flat, verbatim copy-through |
| opencode | `<base>/commands/<plugin>-<stem>.md` | loader recurses `**/*.md` |
| qwen-code | `commands/<plugin>/<rel>.md` | recursive; path segments become `:` namespace segments |

## Gotchas

- **crush commands are palette-only.** They load into the interactive TUI, not `crush run`, so a
  headless crush invocation never sees them.
- **devin and cline remap the surface.** devin turns commands into skills, cline into workflows —
  same content, different home in that tool.
- **Frontmatter fidelity varies.** cursor/crush/cline strip the CC frontmatter (their loaders don't
  split it); gemini/qwen-code re-render into TOML/namespaced forms.
- **Real but untranslated.** `pi` (`~/.pi/agent/prompts/*.md`) and `vscode-copilot`
  (`.github/prompts/*.prompt.md`) have a command surface agentgear does not yet target.

## See also

- [Capabilities](Capabilities) — the index.
- [Agents](Capability-Agents) · [Skills](Capability-Skills) — the sibling markdown surfaces.
- [Harness comparison](Harness-Comparison) — the harness-first grid.
