# Agent backends

The crate installs a plugin into a coding agent through an `AgentBackend`. One binary can
target several agents; `install` loops the configured `agents` list and reconciles each
one. 25 backends ship: Claude Code plus 24 config-merge backends.

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
`self_heal` can run a marker × state table per agent. Each backend defines what
"converged" means for its agent:

- Claude Code: marketplace present, plugin installed, version at or above the embedded one,
  files on disk.
- A config-merge agent (every backend below): its config file matches what the plugin
  declares.

`capabilities()` lets `setup` report a partial fit ("this agent hosts MCP servers, not
hooks") instead of dropping features without a word. The trait is unsealed: an external
crate can write an `AgentBackend` for an agent this crate does not ship.

## Install models

Two shapes:

- **Claude Code.** The `claude` backend orchestrates the `claude plugin` CLI: materialize
  the embedded tree, add or update the marketplace source, install or update the plugin,
  read the result back through `list --json`. Full mcp, hooks, commands, agents, and
  skills. Details on [How It Works](How-It-Works).
- **The 24 config-merge agents.** None of these have a plugin or marketplace concept, so
  each backend read-modify-writes the tool's own config file instead: mcp servers keyed by
  name, hooks/commands/agent defs translated under a plugin-name-prefixed path. Only the
  entries agentgear wrote get touched, so the user's own config survives. A backend writes
  only when `detect()` finds the tool installed and it has a surface at the target scope.

## Coverage

| agent | config | translated |
|---|---|---|
| codex | `~/.codex/config.toml` | mcp, hooks*, commands, agents |
| opencode | `~/.config/opencode/opencode.json` | mcp, commands, agents |
| gemini | `~/.gemini/settings.json` | mcp, hooks, commands |
| cursor | `~/.cursor/mcp.json` + `hooks.json` | mcp, hooks, commands, agents |
| cline | `cline_mcp_settings.json` (path varies) | mcp, hooks, commands |
| devin (Devin Local) | `~/.config/devin/config.json` | mcp, hooks, commands, agents |
| qwen-code | `~/.qwen/settings.json` + `commands/`, `agents/` | mcp, hooks, commands, agents |
| copilot-cli | `~/.copilot/mcp-config.json` + `hooks/`, `agents/` | mcp, hooks, agents |
| vscode-copilot | `<project>/.vscode/mcp.json` + `.github/` (project scope only) | mcp, hooks, agents |
| jetbrains-copilot | `<config>/github-copilot/intellij/mcp.json` | mcp |
| kimi | `~/.kimi-code/mcp.json` + `config.toml` | mcp, hooks |
| kiro | `~/.kiro/settings/mcp.json` + `agents/default.json` | mcp, hooks |
| zed | `~/.config/zed/settings.json` | mcp |
| omp | `~/.omp/agent/mcp.json` + `commands/`, `agents/` | mcp, commands, agents |
| openclaw | `~/.openclaw/openclaw.json` | mcp |
| kilo | `~/.config/kilo/kilo.json` + `commands/`, `agents/` | mcp, commands, agents |
| antigravity | `~/.gemini/config/mcp_config.json` | mcp |
| antigravity-cli | `~/.gemini/config/mcp_config.json` + `~/.gemini/antigravity-cli/hooks.json` | mcp, hooks |
| pi | none (detect-only) | none |
| goose | `~/.config/goose/config.yaml` + `~/.agents/plugins/<plugin>/hooks/` | mcp, hooks |
| amp | `~/.config/amp/settings.json` | mcp |
| crush | `~/.config/crush/crush.json` | mcp, hooks |
| droid | `~/.factory/` (`mcp.json`, `hooks.json`, `commands/`, `droids/`) | mcp, hooks, commands, agents |
| augment | `~/.augment/settings.json` + `commands/`, `agents/` | mcp, hooks, commands, agents |

\* codex hooks are written but stay inert until a user trusts them in codex's `/hooks`
TUI. kimi has no such gate; its config hooks fire as soon as they are written
(binary-verified against `@moonshot-ai/kimi-code` 0.24.2). The July 2026 verification
found that hook writes for kiro, antigravity-cli, and cline's user scope currently land
where those tools never read them; fixes are queued, treat hooks on those three as not
yet functional.

Skills have no backend yet. Remote (http/sse) mcp fidelity varies per tool: most read the
rendered shape as-is, a few key the transport off other fields or reject it whole, so
remote servers stay best-effort until per-tool fixes land. stdio is the tested path
everywhere. Each backend's exact config paths, event-name mapping, and skipped surfaces
live in its own module (`agents/<id>.rs`).

The original six (codex, opencode, gemini, cursor, cline, devin) are verified against their
real tool CLI: a docker leg installs the tool, runs `setup`, confirms the plugin's MCP
server through the tool's own `mcp list`, then checks uninstall removes only what agentgear
wrote while a seeded foreign entry survives. All six pass. The 18 newer backends are covered
by hermetic config-file tests; their docker legs (13 of them; the GUI/IDE and no-surface
backends have none) are authored and first run on CI push. On top of the test suites, every
backend's documented behavior was re-verified against the real shipping tool in July 2026
(scratch-home installs with negative-control probes; the IDE-bound backends via their
shipped extension source).

## Add a backend

`AgentBackend` is the whole seam: six methods, one registry arm.

```rust
pub trait AgentBackend {
    fn id(&self) -> &'static str;
    /// Is this agent installed on the host?
    fn detect(&self) -> bool;
    /// What this agent can host (plugins / mcp / hooks / scopes).
    fn capabilities(&self) -> Capabilities;
    /// Classify this plugin's current state (self_heal's input).
    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState>;
    /// Idempotent converge to `desired` at `scope`.
    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome>;
    /// Undo the install (the caller owns the stamp marker).
    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome>;
    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport;
}
```

### In this crate

1. Add a feature in `Cargo.toml` and a module under `src/agents/` — one file per
   backend. `agents/amp.rs` (mcp-only) is the smallest real template to copy.
2. Implement the trait against the shared writers: `confedit` (atomic, BOM-safe
   json/toml/yaml read-modify-write), `mcpjson` / `mcptoml` (the common server
   shapes). Keep the invariants every backend holds: write only entries the
   plugin owns; `remove` deletes exactly what `reconcile` wrote; a second
   `reconcile` is a true `NoOp`; skip a surface the tool lacks instead of
   writing under a guessed key.
3. Add the id arm in `backend_for` (`src/agents/mod.rs`) and a hermetic
   lifecycle test against a temp `HOME` (copy any `crates/host-fixture/tests/<id>.rs`).

A host opts a plugin into the backend by naming it in the derive:
`agents = ["claude", "mytool"]`.

### From an external crate

The trait is public, so a host can drive a backend this crate does not ship.
It runs beside the built-in fan-out, not inside it:

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

Two limits today: the id registry is closed, so the derive's `agents = [...]`
cannot name an external backend (it never joins `install`/`self_heal`'s locked,
stamped fan-out), and the plugin-tree → components parser is crate-private, so
an external backend reads the plugin tree itself.
