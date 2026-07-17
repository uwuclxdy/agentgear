# Agent backends

The crate installs a plugin into a coding agent through an `AgentBackend`. One binary can target
several agents; `install` loops the configured `agents` list and reconciles each one. 25 backends
ship: Claude Code plus 24 config-merge backends. All 24 non-CC backends were verified against their
real shipping binary on 2026-07-16.

For the side-by-side cross-view (which backend translates what, config paths, remote fidelity,
native Claude-Code-config interop) see [Harness comparison](Harness-Comparison). This page holds the
trait mechanics, backend-specific detail, and how to add your own.

## The trait

```rust
pub enum BackendState { Absent, Healthy, Disabled, NeedsRepair }

pub trait AgentBackend {
    fn id(&self) -> &'static str;
    fn detect(&self) -> bool;                                 // is this agent installed?
    fn capabilities(&self) -> Capabilities;                   // plugins / mcp / hooks / scopes
    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState>;  // self_heal's input
    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome>;
    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome>;
    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport;
}
```

`reconcile` is the seam. Every lifecycle op (`install`, `update`, `self_heal`) reduces to a
reconcile with a different desired state. `probe` classifies the plugin's current state so
`self_heal` can run a marker × state table per agent. Each backend defines what "converged" means:

- Claude Code: marketplace present, plugin installed, version at or above the embedded one, files on
  disk.
- A config-merge agent: its config file matches what the plugin declares.

`capabilities()` lets `setup` report a partial fit ("this agent hosts MCP servers, not hooks")
instead of dropping features silently. The trait is unsealed: an external crate can write an
`AgentBackend` for an agent this crate does not ship.

## Install models

Two shapes:

- **Claude Code.** The `claude` backend orchestrates the `claude plugin` CLI: materialize the
  embedded tree, add or update the marketplace source, install or update the plugin, read the result
  back through `list --json`. Full mcp, hooks, commands, agents, and skills. Details on
  [How It Works](How-It-Works).
- **The 24 config-merge agents.** None have a plugin or marketplace concept, so each backend
  read-modify-writes the tool's own config file: mcp servers keyed by name, hooks/commands/agent
  defs translated under a plugin-name-prefixed path. Only the entries agentgear wrote get touched,
  so the user's own config survives. A backend writes only when `detect()` finds the tool installed
  and it has a surface at the target scope.

## Environment overrides

Where a backend honors a config-dir env var, a test (or a real host) can redirect it. The
**backend ignores** column lists a tool env the real tool honors but agentgear does not yet, so a
host running under it writes where the tool never reads.

| backend | honored | backend ignores (known limitation) |
|---|---|---|
| codex | `CODEX_HOME` (replaces the config dir) | — |
| copilot-cli | `COPILOT_HOME` (replaces the whole path) | — |
| kimi | `KIMI_CODE_HOME` (the config dir) | — |
| kiro | `KIRO_HOME` (points at the `.kiro`-equivalent dir) | — |
| qwen-code | `QWEN_HOME` (used directly, no `.qwen` join) | — |
| crush | `CRUSH_GLOBAL_CONFIG` (dir) | — |
| openclaw | `OPENCLAW_CONFIG_PATH` → `OPENCLAW_STATE_DIR` (flat) → `OPENCLAW_HOME` (reads `<override>/.openclaw/`) | — |
| pi | `PI_CODING_AGENT_DIR` (replaces wholesale) | — |
| omp | `PI_CONFIG_DIR` (renames the dir under HOME; an absolute value still lands under HOME, matching the tool) | — |
| cline | `CLINE_MCP_SETTINGS_PATH` (full path) → `CLINE_DATA_DIR` → `CLINE_DIR` (the store root) | — |
| amp | `XDG_CONFIG_HOME`, else `$HOME/.config` | — |
| goose | `GOOSE_PATH_ROOT` (first, unconditional; relocates config + the plugins/hooks dir) → `XDG_CONFIG_HOME` | — |
| zed | `XDG_CONFIG_HOME` (Linux/FreeBSD; macOS matches zed's hardcoded `~/.config/zed`) | — |
| jetbrains-copilot | `XDG_CONFIG_HOME` (that branch drops the `intellij` segment, matching the plugin's own resolver) | — |
| kilo, devin | `XDG_CONFIG_HOME` | — |
| gemini, cursor, droid, augment | HOME-based, no env override | — |

## Hook event mapping

Most hook-capable backends reuse Claude Code's PascalCase event names, so agentgear maps them 1:1
where the analog exists and skips events with no analog. Five backends rename the events:

| CC event | gemini | cursor | copilot-cli | antigravity-cli | augment |
|---|---|---|---|---|---|
| SessionStart | SessionStart | sessionStart | sessionStart | — | SessionStart |
| SessionEnd | SessionEnd | sessionEnd | sessionEnd | — | SessionEnd |
| UserPromptSubmit | BeforeAgent | beforeSubmitPrompt | userPromptSubmitted | PreInvocation | PromptSubmit |
| PreToolUse | BeforeTool | preToolUse | preToolUse | PreToolUse | PreToolUse |
| PostToolUse | AfterTool | postToolUse | postToolUse | PostToolUse | PostToolUse |
| Stop | — | stop | agentStop | Stop | Stop |
| SubagentStop | — | subagentStop | subagentStop | — | — |
| PreCompact | PreCompress | preCompact | preCompact | — | — |
| Notification | Notification | — | notification | — | Notification |

Per-backend hook notes:

- **codex**: hooks are written but stay inert until a user trusts them in codex's `/hooks` TUI (a
  content-hash trust gate), not a bug.
- **kimi**: no trust gate; config hooks fire as soon as they are written.
- **gemini**: `BeforeTool`/`AfterTool` take a CC tool-name matcher that never matches gemini's own
  tool names (known limitation).
- **cline**: hooks land in dirs the CLI scans at both scopes: `~/Documents/Cline/Hooks/<Event>`
  (user) and `.clinerules/hooks/<Event>` (project). `Hooks` is a sibling of `Rules`, not a child.
- **kiro**: hooks are declared unsupported. kiro hosts hooks only inside user-owned per-agent
  config files, and its run-default agent is a setting rather than a file, so there is no target
  agentgear can own without editing the user's agent definitions.
- **antigravity-cli**: only five event names are legal (`PreToolUse`, `PostToolUse`,
  `PreInvocation`, `PostInvocation`, `Stop`), so CC's `SessionStart` is skipped and
  `UserPromptSubmit` lands on `PreInvocation`. The two tool events take a grouped
  `{matcher, hooks:[…]}` wrapper; a matcher-less CC hook groups under `*`.

## MCP shapes

stdio is the verified path on every backend. The rendered server body varies by tool: `{command,
args, env}` for the json-`mcpServers` family; `type:"stdio"` prefixed for cursor/jetbrains/vscode;
`type:"local"` with an array command for opencode/kilo; `type:"local"` plus `tools:["*"]` for
copilot-cli (a bare `"*"` string voids copilot's whole file); a `[mcp_servers.<name>]` inline table
for codex; a yaml `extensions.<name>` block for goose.

Remote (http/sse) servers render in each tool's own dialect: the majority take
`{type, url, headers}`, cline gets its literal `streamableHttp` value, kimi/devin their
`transport` key, qwen-code its key-presence form (`httpUrl` vs `url`), the antigravity family
its `{serverUrl}` SSE form, zed its bare `{url, headers}`. A transport a tool cannot host
faithfully is skipped rather than written wrong (antigravity http, goose/zed/vscode-copilot
sse). The per-tool verdicts are on [Harness comparison](Harness-Comparison#remote-mcp-fidelity).
Servers whose command or args carry `${CLAUDE_PLUGIN_ROOT}` are skipped on non-CC backends
(that token only expands inside Claude Code).

## Add a backend

`AgentBackend` is the whole seam: seven methods, one registry arm. A host opts a plugin into the
backend by naming it in the derive: `agents = ["claude", "mytool"]`.

### In this crate

1. Add a feature in `Cargo.toml` and a module under `src/agents/`, one file per backend.
   `agents/amp.rs` (mcp-only) is the smallest real template to copy.
2. Implement the trait against the shared writers: `confedit` (atomic, BOM-safe json/toml/yaml
   read-modify-write), `mcpjson` / `mcptoml` (the common server shapes). Keep the invariants every
   backend holds: write only entries the plugin owns; `remove` deletes exactly what `reconcile`
   wrote; a second `reconcile` is a true `NoOp`; skip a surface the tool lacks instead of writing
   under a guessed key.
3. Add the id arm in `backend_for` (`src/agents/mod.rs`) and a hermetic lifecycle test against a
   temp `HOME` (copy any `crates/host-fixture/tests/<id>.rs`).

### From an external crate

The trait is public, so a host can drive a backend this crate does not ship. It runs beside the
built-in fan-out, not inside it:

```rust
use agentgear::{AgentBackend, Desired, PluginHost, Scope, Source};

let plugin = MyHost::descriptor();
MyHost::install(Scope::User, Source::Embedded)?; // the built-in agents
MyToolBackend.reconcile(
    &plugin,
    &Desired { source: Source::Embedded, reenable: true },
    &Scope::User,
)?; // yours
```

Two limits today: the id registry is closed, so the derive's `agents = [...]` cannot name an
external backend (it never joins `install`/`self_heal`'s locked, stamped fan-out), and the
plugin-tree → components parser is crate-private, so an external backend reads the plugin tree
itself.
