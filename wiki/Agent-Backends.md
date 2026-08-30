# Agent backends

The crate installs a plugin into a coding agent through an `AgentBackend`. One binary can target
several agents; `install` loops the configured `agents` list and reconciles each one. 25 backends
ship: Claude Code, copilot-cli (both plugin-native), and 23 config-merge backends. All 24 non-CC
backends were verified against their real shipping binary on 2026-07-16 (copilot-cli's native
rewrite 2026-07-18).

For the harness-first at-a-glance (which backend translates what, config paths, scopes) see
[Harness comparison](Harness-Comparison); for the capability-first depth (MCP shapes + fidelity, hook
events, native Claude-Code-config interop) see [Capabilities](Capabilities). This page holds the trait
mechanics, backend-specific detail, and how to add your own.

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
  tree itself, so there is no per-surface translation and no write outside that model (the one
  former exception — a host-declared [status line](Capability-Status-Line) in the user's own
  `settings.json` — is retired; users wire a line manually). copilot-cli is user-scope only (no
  `--scope` on the `copilot` CLI) and can't pin a GitHub ref (`owner/repo@ref` is misparsed; only
  the bare repo registers, tracking copilot's default branch). Claude Code details:
  [How It Works](How-It-Works).
- **The 23 config-merge agents.** None have a plugin or marketplace concept, so each backend
  read-modify-writes the tool's own config file: mcp servers keyed by name, hooks/commands/agent
  defs translated under a plugin-name-prefixed path. Only the entries agentgear wrote get touched,
  so the user's own config survives. A backend writes only when `detect()` finds the tool installed
  and it has a surface at the target scope.

## Per-capability detail

The deep cross-harness tables that used to live here now sit on the capability pages, one home per
surface:

- **Environment overrides** — the config-dir env var each backend honors, for redirecting a
  hermetic run: [Detection § environment overrides](Capability-Detection#environment-overrides).
- **Hook event mapping** — the CC-event → per-harness-event map, the renames, and the per-backend
  hook notes: [Hooks § event map](Capability-Hooks#event-map).
- **MCP shapes** — the stdio body per tool, the remote dialects, and the fidelity verdicts:
  [MCP](Capability-MCP).

See [Capabilities](Capabilities) for the full index.

## Customization ceiling

A host that uses the derive customizes through the `#[plugin(..)]` attributes plus
`install_into` for runtime agent selection. Every knob configures the whole plugin. None
reshapes one backend: nothing overrides how a backend renders (a different MCP command for
codex) or drops a surface for one backend (skipping hooks on cursor). Each backend writes what
its target tool supports, gated by detection and the `${CLAUDE_PLUGIN_ROOT}` portability filter.

`instructions_fn` is the only method override the derive exposes, because it
emits the sole `impl PluginHost`. For anything past the attributes, write `impl PluginHost` by hand. Every trait
item is public: supply the five consts (`NAME`, `MARKETPLACE`, `VERSION`, `DEFAULT_SOURCE`,
`AGENTS`) and `embedded_blob()`, and the lifecycle methods (`install`, `update`, `self_heal`,
`doctor`, …) come with the trait. You give up three derive-only guarantees:

- the compile error when the `build.rs` guard is missing;
- the `agents`-vs-feature check;
- the baked `include_bytes!` tree: a hand-written `embedded_blob()` returns `&[]`, so such a
  host installs from `Source::Path` or `Source::GitHub`, not `Source::Embedded`.

## Add a backend

`AgentBackend` is the whole seam: seven methods plus a defaulted `forget`, one registry arm. A host opts a plugin into an
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
`Capabilities` is a plain struct, so a field added to it is a breaking change for an external impl
that constructs one — the retired `statusline` field was the first, and pre-1.0 the crate spent that rather than
reshaping the struct.
An external backend maps its own failures to `Error::Backend { agent, detail }` instead of
borrowing an in-crate variant's meaning, builds `report()` from the now-`pub`
`DoctorReport::from_checks` (or `from_error` on an upfront failure), and renders from
`Plugin::components(&source)`, the same harness-agnostic IR the in-crate backends parse from,
rather than walking the plugin tree by hand. Chain `.with_client(id)` on the result to expand
`${AGENTGEAR_CLIENT}` in hook and MCP server commands the same way the in-crate backends do.
Field-level detail on both is on [Types and errors](Types-and-Errors).

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
