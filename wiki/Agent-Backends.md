# Agent backends

The crate installs a plugin into a coding agent through an `AgentBackend`. One binary can target several agents; `install` loops the configured `agents` list and reconciles each one.

## The trait

```rust
pub trait AgentBackend {
    fn id(&self) -> &'static str;
    fn detect(&self) -> bool;                 // is this agent installed?
    fn capabilities(&self) -> Capabilities;   // plugins / mcp / hooks / scopes
    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome>;
    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome>;
    fn report(&self, plugin: &Plugin, source: Source) -> DoctorReport;
}
```

`reconcile` is the seam. Every lifecycle op (`install`, `update`, `self_heal`) reduces to a reconcile with a different desired state. Each backend defines what "converged" means for its agent:

- Claude Code: marketplace present, plugin installed, version at or above the embedded one, files on disk.
- An agent without a marketplace concept: its config bytes match what the plugin declares.

`capabilities()` lets `setup` report a partial fit ("this agent hosts MCP servers, not hooks") instead of dropping features without a word.

## Claude Code (implemented)

The `claude` backend orchestrates the `claude plugin` CLI: materialize the embedded tree, add or update the marketplace source, install or update the plugin, then read the result back through `list --json`. Details are on [How It Works](How-It-Works).

## Other agents (sealed seam)

The `codex` and `opencode` features are reserved backend seams with no implementation in v1. The trait is **sealed**: external crates cannot implement it yet. One real backend is not enough evidence to freeze the contract. Freezing a guessed trait would cost a semver major the moment a second agent needs a different shape.

A second backend unseals the trait. It would:

1. Add a feature and a module under `agents/`.
2. Implement `AgentBackend` for the new agent, writing whatever that agent supports.
3. Register in the backend lookup keyed by id.

Once registered, a host opts a plugin into the new agent by naming it in the derive, for example `agents = ["claude", "codex"]`.
