//! Shared json-`mcpServers` renderer/reconciler for the json family
//! (gemini/cursor/cline/devin). opencode + codex render bespoke bodies. Every
//! write goes through [`confedit::json_edit`], so it is atomic and merge-safe:
//! only our own server keys are touched, the user's survive.

use std::path::Path;

use serde_json::{Map, Value};

use super::BackendState;
use super::confedit::{json_edit, json_obj_at};
use crate::components::{McpKind, McpServer};
use crate::error::{Error, Result};
use crate::host::Outcome;

#[derive(Clone, Copy)]
pub(crate) enum StdioShape {
    /// `{command, args, env}` — gemini, cline, devin.
    Plain,
    /// `{type:"stdio", command, args, env}` — cursor.
    Typed,
}

/// The remote (http/sse) dialect. Harnesses agree on the stdio body far more than
/// on the remote one, so the two axes vary independently.
#[derive(Clone, Copy)]
pub(crate) enum RemoteShape {
    /// `{type:"http"|"sse", url, headers:{}}` — the majority dialect.
    TypeUrlHeaders,
    /// Same keys, but the streamable-HTTP discriminator value is `streamableHttp`
    /// — cline's literal-match schema, where a `type:"http"` entry voids the whole
    /// `mcpServers` object (user servers included).
    StreamableHttpValue,
    /// The antigravity family: the only remote form is `{serverUrl}` (SSE; a
    /// `headers` key may ride along once the IR carries any). The schema is
    /// `additionalProperties:false` and refuses `type`/`url` outright — one bad
    /// entry voids the whole file — and http has no landing at all (skipped).
    ServerUrlSseOnly,
}

#[derive(Clone, Copy)]
pub(crate) struct ServerShape {
    pub(crate) stdio: StdioShape,
    pub(crate) remote: RemoteShape,
}

impl ServerShape {
    pub(crate) const fn plain() -> Self {
        Self { stdio: StdioShape::Plain, remote: RemoteShape::TypeUrlHeaders }
    }

    pub(crate) const fn typed() -> Self {
        Self { stdio: StdioShape::Typed, remote: RemoteShape::TypeUrlHeaders }
    }

    pub(crate) const fn with_remote(mut self, remote: RemoteShape) -> Self {
        self.remote = remote;
        self
    }
}

/// The servers this shape actually writes: portable AND renderable. reconcile,
/// probe, and remove all key off this one filter so ownership can never drift
/// between them (§chokepoint).
fn writable(servers: &[McpServer], shape: ServerShape) -> Vec<&McpServer> {
    servers.iter().filter(|s| s.is_portable() && render_server(s, shape).is_some()).collect()
}

/// Render one server body per `shape`; `None` when the dialect has no faithful
/// landing for the server's kind. A `None` server is skipped exactly like a
/// non-portable one — never written, never owned, never removed — so rendering is
/// the single source of truth for what a dialect supports.
pub(crate) fn render_server(server: &McpServer, shape: ServerShape) -> Option<Value> {
    match &server.kind {
        McpKind::Stdio => {
            let mut obj = Map::new();
            if matches!(shape.stdio, StdioShape::Typed) {
                obj.insert("type".into(), Value::from("stdio"));
            }
            obj.insert("command".into(), Value::from(server.command.clone()));
            obj.insert("args".into(), Value::from(server.args.clone()));
            obj.insert("env".into(), env_value(server));
            Some(Value::Object(obj))
        }
        McpKind::Http { url } => remote(shape.remote, "http", url),
        McpKind::Sse { url } => remote(shape.remote, "sse", url),
    }
}

fn remote(shape: RemoteShape, kind: &str, url: &str) -> Option<Value> {
    let mut obj = Map::new();
    match shape {
        RemoteShape::TypeUrlHeaders | RemoteShape::StreamableHttpValue => {
            let type_value = match shape {
                RemoteShape::StreamableHttpValue if kind == "http" => "streamableHttp",
                _ => kind,
            };
            obj.insert("type".into(), Value::from(type_value));
            obj.insert("url".into(), Value::from(url));
            obj.insert("headers".into(), Value::Object(Map::new()));
        }
        RemoteShape::ServerUrlSseOnly => {
            if kind != "sse" {
                return None;
            }
            obj.insert("serverUrl".into(), Value::from(url));
        }
    }
    Some(Value::Object(obj))
}

fn env_value(server: &McpServer) -> Value {
    let env: Map<String, Value> = server.env.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect();
    Value::Object(env)
}

/// Insert/update exactly our servers under `key_path`, leaving others. `NoOp` when
/// the file already matches (no write).
pub(crate) fn reconcile(path: &Path, key_path: &[&str], servers: &[McpServer], shape: ServerShape) -> Result<Outcome> {
    let changed = json_edit(path, |root| {
        let obj = json_obj_at(root, key_path);
        // Skip non-portable/unrenderable servers here so no json backend can forget
        // to (§chokepoint).
        for server in writable(servers, shape) {
            if let Some(body) = render_server(server, shape) {
                obj.insert(server.name.clone(), body);
            }
        }
        Ok(())
    })?;
    Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
}

/// `Absent` if none of our servers are present; `Healthy` if all present and
/// matching; `NeedsRepair` otherwise. (The json family has no disable flag we set.)
pub(crate) fn probe(path: &Path, key_path: &[&str], servers: &[McpServer], shape: ServerShape) -> Result<BackendState> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BackendState::Absent),
        Err(source) => return Err(Error::Io { context: format!("reading {}", path.display()), source }),
    };
    let root: Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::Config { path: path.display().to_string(), detail: e.to_string() })?;
    let obj = navigate(&root, key_path);

    // Only writable servers are ever written, so only they define ownership. With
    // none to install, this renderer has nothing that could be "gone" -> Healthy,
    // never Absent (an Absent here would make self_heal drop a present marker).
    let ours = writable(servers, shape);
    if ours.is_empty() {
        return Ok(BackendState::Healthy);
    }

    let mut present = 0usize;
    let mut matching = 0usize;
    for server in &ours {
        if let Some(existing) = obj.and_then(|o| o.get(&server.name)) {
            present += 1;
            if render_server(server, shape).is_some_and(|r| r == *existing) {
                matching += 1;
            }
        }
    }
    Ok(if present == 0 {
        BackendState::Absent
    } else if matching == ours.len() {
        BackendState::Healthy
    } else {
        BackendState::NeedsRepair
    })
}

/// Remove exactly our server keys under `key_path`, leaving others. Ownership is
/// the same writable filter reconcile uses, so a server we declared but never
/// wrote (non-portable, or unsupported by this dialect) can never shadow-delete a
/// same-named user entry. Conservatively leaves an emptied object in place rather
/// than dropping the file.
pub(crate) fn remove(path: &Path, key_path: &[&str], servers: &[McpServer], shape: ServerShape) -> Result<Outcome> {
    if !path.exists() {
        return Ok(Outcome::NoOp);
    }
    let changed = json_edit(path, |root| {
        if let Some(obj) = navigate_mut(root, key_path) {
            for server in writable(servers, shape) {
                obj.remove(&server.name);
            }
        }
        Ok(())
    })?;
    Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
}

fn navigate<'a>(root: &'a Value, key_path: &[&str]) -> Option<&'a Map<String, Value>> {
    let mut cur = root;
    for key in key_path {
        cur = cur.get(key)?;
    }
    cur.as_object()
}

fn navigate_mut<'a>(root: &'a mut Value, key_path: &[&str]) -> Option<&'a mut Map<String, Value>> {
    let mut cur = root;
    for key in key_path {
        cur = cur.get_mut(key)?;
    }
    cur.as_object_mut()
}

#[cfg(test)]
#[path = "../../tests/unit/mcpjson.rs"]
mod mcpjson_tests;
