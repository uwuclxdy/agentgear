# Agent backends

The crate installs a plugin into a coding agent through an `AgentBackend`. One binary can target
several agents; `install` loops the configured `agents` list and reconciles each one. 25 backends
ship: Claude Code, copilot-cli (both plugin-native), and 23 config-merge backends. All 24 non-CC
backends were verified against their real shipping binary on 2026-07-16 (copilot-cli's native
rewrite 2026-07-18).

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
    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState>;  // self_heal's input
    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome>;
    fn remove(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<Outcome>;
    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport;
}
```

`reconcile` is the seam. Every lifecycle op (`install`, `update`, `self_heal`) reduces to a
reconcile with a different desired state. `probe` classifies the plugin's current state so
`self_heal` can run a marker × state table per agent; it renders from the `source` argument
(the same one `self_heal` resolves for reconcile), and a backend writing more than one surface
classifies each into an `Option<BackendState>` and folds them through `report::compose`. Each
backend defines what "converged" means:

- Claude Code: marketplace present, plugin installed, version at or above the embedded one, files on
  disk.
- A config-merge agent: its config file matches what the plugin declares.

`capabilities()` lets `setup` report a partial fit ("this agent hosts MCP servers, not hooks")
instead of dropping features silently. The trait is unsealed: an external crate can write an
`AgentBackend` for an agent this crate does not ship.

## Install models

Two shapes:

- **Claude Code and copilot-cli.** Both use the target tool's own plugin-management CLI
  (`claude plugin` / `copilot plugin`): materialize the embedded tree, add or update the
  marketplace source, install or update the plugin, read the result back through the CLI's own
  list command. Full mcp, hooks, commands, agents, and skills, since the tool copies the whole
  tree itself, so there is no per-surface translation. copilot-cli is user-scope only (no
  `--scope` on the `copilot` CLI) and can't pin a GitHub ref (`owner/repo@ref` is misparsed; only
  the bare repo registers, tracking copilot's default branch). Claude Code details:
  [How It Works](How-It-Works).
- **The 23 config-merge agents.** None have a plugin or marketplace concept, so each backend
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
where the analog exists and skips events with no analog. Four backends rename the events:

| CC event | gemini | cursor | antigravity-cli | augment |
|---|---|---|---|---|
| SessionStart | SessionStart | sessionStart | — | SessionStart |
| SessionEnd | SessionEnd | sessionEnd | — | SessionEnd |
| UserPromptSubmit | BeforeAgent | beforeSubmitPrompt | PreInvocation | PromptSubmit |
| PreToolUse | BeforeTool | preToolUse | PreToolUse | PreToolUse |
| PostToolUse | AfterTool | postToolUse | PostToolUse | PostToolUse |
| Stop | — | stop | Stop | Stop |
| SubagentStart | — | subagentStart | — | — |
| SubagentStop | — | subagentStop | — | — |
| PreCompact | PreCompress | preCompact | — | — |
| Notification | Notification | — | — | Notification |

`copilot-cli` is not in this table: it's plugin-native, so the whole CC `hooks/hooks.json` copies
into `~/.copilot/installed-plugins/` verbatim, event names included, rather than going through
`map_event`. Whether the copied raw-shape file actually fires is unconfirmed (no headless
hooks-list command exists).

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
`type:"local"` with an array command for opencode/kilo; a `[mcp_servers.<name>]` inline table for
codex; a yaml `extensions.<name>` block for goose. `copilot-cli` renders no mcp shape at all: its
own plugin engine reads `.claude-plugin/plugin.json` directly.

Remote (http/sse) servers render in each tool's own dialect: the majority take
`{type, url, headers}`, cline gets its literal `streamableHttp` value, kimi/devin their
`transport` key, qwen-code its key-presence form (`httpUrl` vs `url`), the antigravity family
its `{serverUrl}` SSE form, zed its bare `{url, headers}`. A transport a tool cannot host
faithfully is skipped rather than written wrong (antigravity http, goose/zed/vscode-copilot
sse). The per-tool verdicts are on [Harness comparison](Harness-Comparison#remote-mcp-fidelity).
Servers whose command or args carry `${CLAUDE_PLUGIN_ROOT}` are skipped on non-CC backends
(that token only expands inside Claude Code).

## Customization ceiling

A host that uses the derive customizes through the `#[plugin(..)]` attributes plus
`install_into` for runtime agent selection. Every knob configures the whole plugin. None
reshapes one backend: nothing overrides how a backend renders (a different MCP command for
codex) or drops a surface for one backend (skipping hooks on cursor). Each backend writes what
its target tool supports, gated by detection and the `${CLAUDE_PLUGIN_ROOT}` portability filter.

`instructions_fn` is the only method override the derive exposes, because it emits the sole
`impl PluginHost`. For anything past the attributes, write `impl PluginHost` by hand. Every trait
item is public: supply the five consts (`NAME`, `MARKETPLACE`, `VERSION`, `DEFAULT_SOURCE`,
`AGENTS`) and `embedded_blob()`, and the lifecycle methods (`install`, `update`, `self_heal`,
`doctor`, …) come with the trait. You give up three derive-only guarantees:

- the compile error when the `build.rs` guard is missing;
- the `agents`-vs-feature check;
- the baked `include_bytes!` tree: a hand-written `embedded_blob()` returns `&[]`, so such a
  host installs from `Source::Path` or `Source::GitHub`, not `Source::Embedded`.

## Add a backend

`AgentBackend` is the whole seam: seven methods, one registry arm. A host opts a plugin into an
in-crate backend by naming its id in the derive's `agents = [...]` list.

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

The trait is unsealed: every method, `probe`, `reconcile`, `remove`, `report` included,
is implementable from outside the crate, not only the read-only ones a thin wrapper could reach.
An external backend maps its own failures to `Error::Backend { agent, detail }` instead of
borrowing an in-crate variant's meaning, builds `report()` from the now-`pub`
`DoctorReport::from_checks` (or `from_error` on an upfront failure), and renders from
`Plugin::components(&source)`, the same harness-agnostic IR the in-crate backends parse from,
rather than walking the plugin tree by hand. Field-level detail on both is on
[Types and errors](Types-and-Errors).

The id registry `backend_for` resolves stays closed, so the derive's `agents = [...]` still cannot
name an external backend. It never joins the locked, stamped `install`/`self_heal` fan-out; it
runs beside that fan-out instead, driven directly:

```rust
use agentgear::{AgentBackend, Desired, PluginHost, Scope, Source};

let plugin = MyHost::descriptor();
MyHost::install(Scope::User, Source::Embedded)?; // the built-in agents
let source = Source::Embedded;
let components = plugin.components(&source)?; // the parsed plugin IR
let desired = Desired { source: source.clone(), reenable: true };
MyToolBackend.reconcile(&plugin, &desired, &Scope::User)?; // yours, rendering from `components`
MyToolBackend.remove(&plugin, &Scope::User, &source)?; // symmetric with reconcile
```
