//! Codex's toml `[mcp_servers.<name>]` renderer/reconciler, over
//! [`confedit::toml_edit`]. Bespoke: codex hosts mcp in `config.toml`, not the
//! shared json `mcpServers` object. Every write is merge-safe — only our own
//! `[mcp_servers.<name>]` sub-tables are inserted/updated; the user's other servers,
//! key order, and every unrelated top-level table survive.
//!
//! Comment survival is narrower than key survival, and only on the removal side.
//! `toml_edit` attaches a comment to the item it precedes, so one written above an
//! `[mcp_servers…]` header goes out with that header — whether that is our own
//! sub-table or the `[mcp_servers]` container we prune once it holds nothing of ours.
//! A comment anywhere else in the file is untouched. And a `config.toml` our removal
//! empties outright is deleted ([`confedit::toml_remove`]), taking any comment left in
//! it: our removal having emptied every key means it was already orphaned.

use std::collections::BTreeMap;
use std::path::Path;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

use super::BackendState;
use super::confedit;
use crate::components::{McpKind, McpServer};
use crate::error::{Error, Result};
use crate::host::Outcome;

/// Insert/update exactly our (portable) servers under `[mcp_servers]`, leaving the
/// user's. `NoOp` when the file already matches (no write). `reenable=false`
/// (self_heal) preserves an existing `enabled = false` a user set by hand; a whole
/// plugin declaring no portable server is a no-op (no empty table written).
pub(crate) fn reconcile(path: &Path, servers: &[McpServer], reenable: bool) -> Result<Outcome> {
    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    if portable.is_empty() {
        return Ok(Outcome::NoOp);
    }
    let changed = confedit::toml_edit(path, |doc| {
        let Some(table) = mcp_table(doc) else {
            return Ok(()); // user has a non-table `mcp_servers`; never clobber it
        };
        for server in &portable {
            let existing = table.get(&server.name);
            let currently_disabled = existing.is_some_and(server_disabled);
            let want_disabled = !reenable && currently_disabled;
            // Skip when the on-disk table already equals what we'd write, so a
            // second reconcile touches no bytes regardless of toml_edit's decor.
            if let Some(existing) = existing
                && fields_match(existing, server)
                && server_disabled(existing) == want_disabled
            {
                continue;
            }
            table.insert(&server.name, Item::Table(render_table(server, want_disabled)));
        }
        Ok(())
    })?;
    Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
}

/// `Absent` if none of our servers are present; `Disabled` if all present ones
/// match with `enabled = false` (a user's deliberate codex-level disable self_heal
/// must never flip); `Healthy` if all present and matching; `NeedsRepair` otherwise.
/// `Healthy` (not `Absent`) for an mcp-less plugin, so a present marker is not dropped.
pub(crate) fn probe(path: &Path, servers: &[McpServer]) -> Result<BackendState> {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BackendState::Absent),
        Err(source) => return Err(Error::Io { context: format!("reading {}", path.display()), source }),
    };
    let doc: DocumentMut =
        text.parse().map_err(|e: toml_edit::TomlError| Error::Config { path: path.display().to_string(), detail: e.to_string() })?;

    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    if portable.is_empty() {
        return Ok(BackendState::Healthy);
    }
    let table = doc.get("mcp_servers").and_then(Item::as_table);
    let (mut present, mut healthy, mut disabled) = (0usize, 0usize, 0usize);
    for server in &portable {
        if let Some(item) = table.and_then(|t| t.get(&server.name)) {
            present += 1;
            if fields_match(item, server) {
                if server_disabled(item) {
                    disabled += 1;
                } else {
                    healthy += 1;
                }
            }
        }
    }
    Ok(if present == 0 {
        BackendState::Absent
    } else if disabled == portable.len() {
        BackendState::Disabled
    } else if healthy == portable.len() {
        BackendState::Healthy
    } else {
        BackendState::NeedsRepair
    })
}

/// The composition-aware form of [`probe`]: `None` when the plugin declares no
/// portable server, so the mcp surface contributes no verdict to
/// [`super::report::compose`]; otherwise `Some(probe(...))`.
pub(crate) fn probe_surface(path: &Path, servers: &[McpServer]) -> Result<Option<BackendState>> {
    if !servers.iter().any(McpServer::is_portable) {
        return Ok(None);
    }
    Ok(Some(probe(path, servers)?))
}

/// Remove exactly our server keys from `[mcp_servers]`, leaving others. The table
/// goes with them once ours were the last keys in it — the exact inverse of the
/// [`mcp_table`] that created it — and a `config.toml` left holding nothing goes too.
///
/// Pruning the table is what makes the file arm honest: `mcp_table` creates it
/// implicit, so an emptied one renders to zero bytes while still keying the root, and
/// a naive root test would read that 0-byte file as a document worth keeping.
pub(crate) fn remove(path: &Path, names: &[&str]) -> Result<Outcome> {
    if !path.exists() || names.is_empty() {
        return Ok(Outcome::NoOp);
    }
    let changed = confedit::toml_remove(path, |doc| {
        confedit::toml_prune(doc, "mcp_servers", |item| {
            if let Some(table) = item.as_table_mut() {
                for name in names {
                    table.remove(name);
                }
            }
            Ok(())
        })
    })?;
    Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
}

// --- rendering ---------------------------------------------------------------

/// The `[mcp_servers]` table, created implicit (no bare `[mcp_servers]` header) if
/// absent. `None` only when the user's `mcp_servers` key exists as a non-table.
fn mcp_table(doc: &mut DocumentMut) -> Option<&mut Table> {
    let item = doc.as_table_mut().entry("mcp_servers").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    item.as_table_mut()
}

/// One `[mcp_servers.<name>]` body: `command`/`args`(+`env`) for stdio, `url` for
/// remote. `enabled = false` is emitted only when we're preserving a user disable.
fn render_table(server: &McpServer, disabled: bool) -> Table {
    let mut t = Table::new();
    match &server.kind {
        McpKind::Stdio => {
            t.insert("command", value(server.command.as_str()));
            let mut args = Array::new();
            for a in &server.args {
                args.push(a.as_str());
            }
            t.insert("args", value(args));
            if !server.env.is_empty() {
                let mut env = InlineTable::new();
                for (k, v) in &server.env {
                    env.insert(k.as_str(), v.as_str().into());
                }
                t.insert("env", value(env));
            }
        }
        // Codex's remote mcp is streamable-HTTP keyed by `url`; SSE has no distinct
        // codex shape, so both remote kinds render the same `url` field (best-effort).
        McpKind::Http { url } | McpKind::Sse { url } => {
            t.insert("url", value(url.as_str()));
        }
    }
    if disabled {
        t.insert("enabled", value(false));
    }
    t
}

fn server_disabled(item: &Item) -> bool {
    item.as_table_like().and_then(|t| t.get("enabled")).and_then(Item::as_bool) == Some(false)
}

/// Whether an on-disk server table's payload (ignoring `enabled`) equals what we'd
/// render: drives drift detection in `probe` and the idempotent skip in `reconcile`.
fn fields_match(item: &Item, server: &McpServer) -> bool {
    let Some(t) = item.as_table_like() else {
        return false;
    };
    match &server.kind {
        McpKind::Stdio => {
            let command_ok = t.get("command").and_then(Item::as_str) == Some(server.command.as_str());
            let args: Vec<&str> =
                t.get("args").and_then(Item::as_array).map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
            let want: Vec<&str> = server.args.iter().map(String::as_str).collect();
            command_ok && args == want && env_matches(t.get("env"), &server.env)
        }
        McpKind::Http { url } | McpKind::Sse { url } => t.get("url").and_then(Item::as_str) == Some(url.as_str()),
    }
}

fn env_matches(item: Option<&Item>, env: &BTreeMap<String, String>) -> bool {
    let existing: BTreeMap<String, String> = match item.and_then(Item::as_table_like) {
        Some(t) => t.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.to_string(), s.to_string()))).collect(),
        None => BTreeMap::new(),
    };
    existing == *env
}
