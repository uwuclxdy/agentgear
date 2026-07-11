# Agent backends

The crate installs a plugin into a coding agent through an `AgentBackend`. One binary can
target several agents; `install` loops the configured `agents` list and reconciles each
one. Seven backends ship: Claude Code, codex, opencode, gemini, cursor, cline, and devin
(Devin Local).

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
- A config-merge agent (codex, opencode, gemini, cursor, cline, devin): its config file
  matches what the plugin declares.

`capabilities()` lets `setup` report a partial fit ("this agent hosts MCP servers, not
hooks") instead of dropping features without a word. The trait is unsealed: an external
crate can write an `AgentBackend` for an agent this crate does not ship.

## Install models

Two shapes:

- **Claude Code.** The `claude` backend orchestrates the `claude plugin` CLI: materialize
  the embedded tree, add or update the marketplace source, install or update the plugin,
  read the result back through `list --json`. Full mcp, hooks, commands, agents, and
  skills. Details on [How It Works](How-It-Works).
- **codex, opencode, gemini, cursor, cline, devin.** None of these have a plugin or
  marketplace concept, so each backend read-modify-writes the tool's own config file
  instead: mcp servers keyed by name, hooks/commands/agent defs translated under a
  plugin-name-prefixed path. Only the entries agentgear wrote get touched, so the user's
  own config survives. A backend writes only when `detect()` finds the tool installed.

## Coverage

| agent | config | translated |
|---|---|---|
| codex | `~/.codex/config.toml` | mcp, hooks*, commands, agents |
| opencode | `~/.config/opencode/opencode.json` | mcp, commands, agents |
| gemini | `~/.gemini/settings.json` | mcp, hooks, commands |
| cursor | `~/.cursor/mcp.json` + `hooks.json` | mcp, hooks, commands, agents |
| cline | `cline_mcp_settings.json` (path varies) | mcp, hooks, commands |
| devin (Devin Local) | `~/.config/devin/config.json` | mcp, hooks, commands, agents |

\* codex hooks are written but stay inert until a user approves them in codex's `/hooks`
TUI.

Skills have no backend yet on any of the six. Each backend's exact event-name mapping and
skipped surfaces are documented in its own module (`agents/<id>.rs` in the crate source).

Every backend in this table is verified against its real tool CLI. A docker leg per
backend installs the tool, runs `setup`, then confirms the plugin's MCP server through the
tool's own `mcp list`. Uninstall then removes only what agentgear wrote; a seeded
foreign entry survives untouched. All six pass.

## Add a backend

1. Add a feature and a module under `agents/`.
2. Write an `AgentBackend` impl for the agent, translating the surfaces it has.
3. Register it in the backend lookup keyed by id.

A host opts a plugin into an agent by naming it in the derive, for example
`agents = ["claude", "codex"]`.
