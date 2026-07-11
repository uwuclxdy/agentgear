//! Codex's toml `[mcp_servers.<name>]` renderer, over `confedit::toml_edit`.
//!
//! STUB: codex backend filled by its workflow — the real reconcile (args/env/
//! timeouts, merge-safe insert) lands here when codex is implemented.

use std::path::Path;

use crate::components::McpServer;
use crate::error::Result;
use crate::host::Outcome;

/// Render one server's `[mcp_servers.<name>]` table body. Minimal stub; the codex
/// workflow fills args/env/timeouts.
pub(crate) fn render(server: &McpServer) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    table["command"] = toml_edit::value(server.command.clone());
    table
}

/// Reconcile our servers into `~/.codex/config.toml`'s `mcp_servers` table.
/// STUB: no-op until the codex workflow wires `confedit::toml_edit`.
pub(crate) fn reconcile(_path: &Path, _servers: &[McpServer]) -> Result<Outcome> {
    Ok(Outcome::NoOp)
}
