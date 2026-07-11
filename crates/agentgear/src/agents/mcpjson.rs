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
pub(crate) enum ServerShape {
    /// `{command, args, env}` — gemini, cline, devin.
    Plain,
    /// `{type:"stdio", command, args, env}` — cursor.
    Typed,
}

/// Render one server body per `shape`. Http/Sse ignore the shape and render
/// `{type, url, headers:{}}`.
pub(crate) fn render_server(server: &McpServer, shape: ServerShape) -> Value {
    match &server.kind {
        McpKind::Stdio => {
            let mut obj = Map::new();
            if matches!(shape, ServerShape::Typed) {
                obj.insert("type".into(), Value::from("stdio"));
            }
            obj.insert("command".into(), Value::from(server.command.clone()));
            obj.insert("args".into(), Value::from(server.args.clone()));
            obj.insert("env".into(), env_value(server));
            Value::Object(obj)
        }
        McpKind::Http { url } => remote("http", url),
        McpKind::Sse { url } => remote("sse", url),
    }
}

fn remote(kind: &str, url: &str) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::from(kind));
    obj.insert("url".into(), Value::from(url));
    obj.insert("headers".into(), Value::Object(Map::new()));
    Value::Object(obj)
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
        // Skip non-portable servers here so no json backend can forget to (§chokepoint).
        for server in servers.iter().filter(|s| s.is_portable()) {
            obj.insert(server.name.clone(), render_server(server, shape));
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

    // Only portable servers are ever written, so only they define ownership. With
    // none to install, this renderer has nothing that could be "gone" -> Healthy,
    // never Absent (an Absent here would make self_heal drop a present marker).
    let portable: Vec<&McpServer> = servers.iter().filter(|s| s.is_portable()).collect();
    if portable.is_empty() {
        return Ok(BackendState::Healthy);
    }

    let mut present = 0usize;
    let mut matching = 0usize;
    for server in &portable {
        if let Some(existing) = obj.and_then(|o| o.get(&server.name)) {
            present += 1;
            if *existing == render_server(server, shape) {
                matching += 1;
            }
        }
    }
    Ok(if present == 0 {
        BackendState::Absent
    } else if matching == portable.len() {
        BackendState::Healthy
    } else {
        BackendState::NeedsRepair
    })
}

/// Remove exactly our server keys under `key_path`, leaving others. Conservatively
/// leaves an emptied object in place rather than dropping the file.
pub(crate) fn remove(path: &Path, key_path: &[&str], server_names: &[&str]) -> Result<Outcome> {
    if !path.exists() {
        return Ok(Outcome::NoOp);
    }
    let changed = json_edit(path, |root| {
        if let Some(obj) = navigate_mut(root, key_path) {
            for name in server_names {
                obj.remove(*name);
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
