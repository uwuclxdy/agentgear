# Agents

An agent is an `agents/**/*.md` file — a named subagent persona with its own instructions. agentgear
copies each into the target tool's own subagent directory, translating the frontmatter (injecting a
`mode: subagent` or a required `name` where the tool needs one) and prefixing the plugin name.

**13 backends host agents.** The rest have no file-writable subagent surface: some expose only
built-in agents (kimi), some model subagents as recipes or isolated workspaces (goose, openclaw),
some a rich JSON schema a markdown translate would lose (kiro), and crush's file surface is still
open upstream (issue #1807).

## Where each agent lands

| harness | where written | notes |
|---|---|---|
| claude | `agents/**/*.md` (native) | — |
| copilot-cli | native, copied verbatim | untranslated CC frontmatter, live-proven |
| augment | `~/.augment/agents/<plugin>-<name>.md` | `name` required, plugin-prefixed |
| codex | `agents/<plugin>-<name>.toml` | `name` / `description` / `developer_instructions` |
| cursor | `~/.cursor/agents/<plugin>-<name>.md` | cursor cross-reads `~/.claude/agents/` + `~/.codex/agents/` |
| devin | `agents/<plugin>-<stem>/AGENT.md` | CC agents land as devin **subagents** |
| droid | `droids/<plugin>-<stem>.md` | flat top-level only |
| gemini | `~/.gemini/agents/<plugin>-<name>.md` + `.gemini/agents/*.md` | schema stable |
| kilo | `<base>/agents/<plugin>-<stem>.md` | `mode: subagent` injected |
| omp | `<base>/agents/<plugin>-<stem>.md` | `description` mandatory — its absence is a **silent drop**. Write **retires** when CC's registry already covers the plugin (see [Native ingestion](Capability-Native-Ingestion)) |
| opencode | `<base>/agents/<plugin>-<stem>.md` | `mode: subagent` injected (its default is `all`) |
| qwen-code | `agents/<plugin>-<name>.md` | keyed by frontmatter `name` (plugin-prefixed) |
| vscode-copilot | `.github/agents/<plugin>-<stem>.agent.md` | YAML frontmatter + body |

## Gotchas

- **omp silent drop.** An agent with no `description` frontmatter is dropped without a word — omp
  requires it. And omp's write retires entirely once Claude Code's registry lists the plugin and
  omp's `claude-plugins` provider is on, to avoid double-registration.
- **cursor's translation also retires** when Claude Code's registry lists the plugin — cursor reads
  the CC plugin tree directly. See [Native ingestion](Capability-Native-Ingestion).
- **Real but unimplemented.** `cline` has an agent surface (`~/.cline/agents/`) whose on-disk shape
  is unverified, so agentgear does not write it yet.

## See also

- [Capabilities](Capabilities) — the index.
- [Native ingestion](Capability-Native-Ingestion) — where an agent translation retires.
- [Commands](Capability-Commands) · [Skills](Capability-Skills) — the sibling markdown surfaces.
