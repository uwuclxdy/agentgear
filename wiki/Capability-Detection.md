# Detection & config roots

Before writing anything, a backend answers two questions: is this tool installed (`detect()`), and
where does its config live? A tool that fails detection is a clean skip — no file, no marker, no
error — and `self_heal` adopts it once it appears. This page maps the detection signal, the user and
project config roots, and the environment override each backend honors.

## Detection signal & config roots

| harness | detection signal | user config root | project config root | env override (precedence) |
|---|---|---|---|---|
| claude | `claude` on PATH (version floor 2.1.196) | `<cfg>/plugins/` | `Scope::Project{path}`, reaches the CLI as cwd | `CLAUDE_CONFIG_DIR` |
| amp | `amp` on PATH, or `~/.config/amp/` | `$XDG_CONFIG_HOME/amp/` else `$HOME/.config/amp/` | `<cwd>/.amp/`, exact CWD, approval-gated | `XDG_CONFIG_HOME` |
| antigravity | `~/.gemini/antigravity-ide/` dir marker only | `~/.gemini/config/` (shared with the CLI + Hub) | none (user-only) | none |
| antigravity-cli | `agy` on PATH, or `~/.gemini/antigravity-cli/` | `~/.gemini/config/` | `<root>/.agents/`, needs folder trust | none |
| augment | `auggie` on PATH, or `~/.augment/` | `~/.augment/` | `<cwd>/.augment/` (user-only backend) | none |
| cline | `cline` on PATH, or `$CLINE_DIR`/`~/.cline`, `~/Documents/Cline`, legacy globalStorage | `$CLINE_DIR` else `~/.cline` + `~/Documents/Cline/` | `.cline/` + `.clinerules/` at repo root | `CLINE_MCP_SETTINGS_PATH` → `CLINE_DATA_DIR` → `CLINE_DIR` |
| codex | `codex` on PATH, or `~/.codex`/`$CODEX_HOME` dir | `~/.codex/` | `<cwd>/.codex/`, trust-gated | `CODEX_HOME` (checked first) |
| copilot-cli | `copilot` on PATH only (version floor 1.0.71) | `~/.copilot/` | none targeted (user-only install) | `COPILOT_HOME` (replaces the whole path) |
| crush | `crush` on PATH, or `~/.config/crush` | `~/.config/crush` (Win `%LOCALAPPDATA%\crush`) | `<project>/crush.json` (walks cwd→git root) | `CRUSH_GLOBAL_CONFIG` (dir) |
| cursor | `cursor-agent`/`cursor` on PATH, else `~/.cursor` | `~/.cursor` (HOME-keyed) | `<cwd>/.cursor`, no gate | none |
| devin | `devin` on PATH, or `~/.config/devin`, or project `.devin/`/`.cognition/` | `~/.config/devin` (Win `%APPDATA%\devin`) | `.devin/` under cwd | — |
| droid | `droid` on PATH, or `~/.factory` is a dir | `~/.factory` (HOME-based) | `<cwd>/.factory` | none |
| gemini | `gemini` on PATH, or `~/.gemini/` | `~/.gemini/` | `<cwd>/.gemini/`, trust-gated | none (`GEMINI_CLI_HOME` is a false lead) |
| goose | `goose` on PATH, or `~/.config/goose` | `~/.config/goose/` (Win `%APPDATA%\Block\goose\config\`) | none (no project extensions file) | `GOOSE_PATH_ROOT` → `XDG_CONFIG_HOME` |
| jetbrains-copilot | no binary; `<config>/github-copilot/intellij/` dir present | `<config>/github-copilot/intellij/` | `<project>/.github/mcp.json` (unwritten) | `XDG_CONFIG_HOME` (drops the `/intellij` suffix) |
| kilo | `kilo` on PATH, or `~/.config/kilo` | `~/.config/kilo/` | `<project>/.kilo/kilo.json` (walks cwd→git root) | `XDG_CONFIG_HOME` |
| kimi | `~/.kimi-code` dir present (no binary probe) | `~/.kimi-code` | `<cwd>/.kimi-code` | `KIMI_CODE_HOME` (checked first) |
| kiro | `kiro-cli` on PATH, or the config base | `~/.kiro` | `<project>/.kiro` | `KIRO_HOME` (points at the `.kiro`-equivalent dir) |
| omp | `omp` on PATH, else `~/.omp` | `~/.omp/agent` | `<cwd>/.omp` | `PI_CONFIG_DIR` (renames the dir under HOME); `OMP_PROFILE`/`PI_PROFILE` relocate |
| openclaw | `openclaw` on PATH, or the state dir | `~/.openclaw/openclaw.json` | none (single user-level file) | `OPENCLAW_CONFIG_PATH` → `OPENCLAW_STATE_DIR` → `OPENCLAW_HOME` |
| opencode | `opencode` on PATH, or `~/.config/opencode` | `~/.config/opencode/` | `<project-root>/opencode.json` or `.opencode/` (walks cwd→git root) | `OPENCODE_CONFIG_DIR` / `OPENCODE_CONFIG` (neither honored) |
| pi | `pi` on PATH, else `~/.pi` dir | `~/.pi/agent` | `.pi/` (nothing written) | `PI_CODING_AGENT_DIR` (replaces wholesale) |
| qwen-code | `qwen` on PATH, or the config dir exists | `$QWEN_HOME` used directly, else `~/.qwen` | `<cwd>/.qwen` | `QWEN_HOME` (the config dir itself, no `.qwen` join) |
| vscode-copilot | `code`/`code-insiders` on PATH, or `~/.vscode/` | none used (profile ambiguity) | repo root: `.vscode/`, `.github/hooks/`, `.github/agents/` | none observable |
| zed | `zed` on PATH, or the config dir | `$XDG_CONFIG_HOME/zed` or `~/.config/zed`; macOS hardcodes `~/.config/zed` | `<worktree>/.zed/settings.json` | `FLATPAK_XDG_CONFIG_HOME` → `XDG_CONFIG_HOME` (Linux) |

## Environment overrides

Where a backend honors a config-dir env var, a test (or a real host) can redirect it to a temp
directory — this is how the hermetic tests point a backend at a scratch `HOME`. The column above lists
the precedence chain each backend follows. agentgear honors the tool's own primary config-dir
override for every backend that has one. A few tools expose additional env vars agentgear does not
read — kilo's `KILO_CONFIG` / `KILO_CONFIG_DIR`, opencode's `OPENCODE_CONFIG_DIR` / `OPENCODE_CONFIG`,
omp's `PI_CODING_AGENT_DIR`, and crush's higher-precedence `$XDG_DATA_HOME/crush/crush.json`
(`CRUSH_GLOBAL_DATA`) — a known limitation if a user sets one. HOME-based backends (gemini, cursor,
droid, augment) take no override.

An override set to the **empty string** is a special case worth knowing, because the tools disagree
about it. `claude` and `copilot` both read an empty value as the config dir itself, resolving
`settings.json` against the current working directory, so agentgear refuses that input: setting
`CLAUDE_CONFIG_DIR=` or `COPILOT_HOME=` fails that agent's install with an error naming the variable,
rather than writing to `~/.claude` or `~/.copilot` where the tool would never look. Other backends
still read an empty override as unset and fall back to their default root; what their tools do with
an empty value has not been probed.

## See also

- [Capabilities](Capabilities) — the index.
- [Scopes & gates](Capability-Scopes) — user vs project and the activation gates.
- [Testing your host](Testing-Your-Host) — redirecting an override for a hermetic run.
