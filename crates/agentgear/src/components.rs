//! The plugin-components IR: a Claude-plugin tree, already flattened to
//! `(rel-path, bytes)` entries, parsed into a harness-agnostic shape every non-CC
//! backend renders from. It is deliberately lossless — a backend that lacks a
//! surface (say hooks) just skips that field instead of the parser dropping it.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// The portability token every backend expands to ITS OWN canonical client id
/// (`claude`, `codex`, `copilot-cli`, ...). Unlike `${CLAUDE_PLUGIN_ROOT}` — which
/// only Claude Code expands, so [`is_portable`](HookBinding::is_portable) flags it as
/// a skip — this token is known to agentgear and expanded per harness in an
/// executable command, so an author writes the client id once and each harness sees
/// its own. Plugin-native backends (CC/copilot) copy the whole tree and substitute it
/// everywhere (see materialize); the config-translating backends expand only the
/// command surfaces they render — hook commands and mcp command/args
/// ([`with_client`](PluginComponents::with_client)).
pub const AGENTGEAR_CLIENT_TOKEN: &str = "${AGENTGEAR_CLIENT}";

/// Replace every [`AGENTGEAR_CLIENT_TOKEN`] in `s` with `client`. A no-op when the
/// token is absent.
pub(crate) fn expand_client(s: &str, client: &str) -> String {
    s.replace(AGENTGEAR_CLIENT_TOKEN, client)
}

/// A plugin tree parsed into a harness-agnostic shape. Lossless: a backend that
/// lacks a surface skips that field, the parser never drops one.
#[derive(Debug, Clone, Default)]
pub struct PluginComponents {
    /// MCP servers declared by the plugin.
    pub mcp_servers: Vec<McpServer>,
    /// Hook bindings across every event.
    pub hooks: Vec<HookBinding>,
    /// Slash commands (`commands/*.md`).
    pub commands: Vec<MarkdownDoc>,
    /// Subagent definitions (`agents/*.md`).
    pub agents: Vec<MarkdownDoc>,
    /// Skill directories (`skills/<name>/`).
    pub skills: Vec<SkillDir>,
}

/// One MCP server the plugin declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    /// Server key; already unique within the plugin.
    pub name: String,
    /// Transport (stdio, http, or sse).
    pub kind: McpKind,
    /// Stdio: the executable. Recorded verbatim; a `${CLAUDE_PLUGIN_ROOT}`-bearing
    /// command is kept as-is but flagged non-portable (see [`McpServer::is_portable`]).
    /// A bare binary is the tested path.
    pub command: String,
    /// Stdio: the executable's arguments.
    pub args: Vec<String>,
    /// Environment variables passed to the server.
    pub env: BTreeMap<String, String>,
}

impl McpServer {
    /// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's plugin
    /// runtime, so a command/arg carrying it cannot run under another harness. Every
    /// non-CC backend skips a non-portable server (the shared json renderer enforces
    /// it; bespoke backends must call this too).
    pub fn is_portable(&self) -> bool {
        const VAR: &str = "${CLAUDE_PLUGIN_ROOT}";
        !self.command.contains(VAR) && !self.args.iter().any(|a| a.contains(VAR))
    }
}

/// An MCP server's transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpKind {
    /// Launched as a subprocess speaking over stdio.
    Stdio,
    /// Streamable-HTTP endpoint.
    Http {
        /// The server URL.
        url: String,
    },
    /// Server-sent-events endpoint.
    Sse {
        /// The server URL.
        url: String,
    },
}

/// One hook binding: a command wired to an event, optionally matcher-gated.
#[derive(Debug, Clone)]
pub struct HookBinding {
    /// `"SessionStart"`, `"UserPromptSubmit"`, ...
    pub event: String,
    /// The event matcher, when the event supports one.
    pub matcher: Option<String>,
    /// Shell command string.
    pub command: String,
}

impl HookBinding {
    /// A `${CLAUDE_PLUGIN_ROOT}` reference only expands inside Claude Code's hook
    /// runner, so a command carrying it would spawn the literal, unexpanded token
    /// under another harness. Every non-CC backend skips a non-portable hook.
    /// Mirrors [`McpServer::is_portable`].
    pub fn is_portable(&self) -> bool {
        !self.command.contains("${CLAUDE_PLUGIN_ROOT}")
    }
}

/// A markdown component (a command or agent) with its frontmatter split out.
#[derive(Debug, Clone)]
pub struct MarkdownDoc {
    /// File stem.
    pub name: String,
    /// Path within the tree (namespacing on write).
    pub rel: String,
    /// Parsed leading `---` frontmatter block.
    pub frontmatter: BTreeMap<String, Value>,
    /// The markdown after the frontmatter.
    pub body: String,
    /// Verbatim file bytes (copy-through when no transform applies).
    pub raw: Vec<u8>,
}

/// A skill directory and its files, ready to copy into a harness's skills dir.
#[derive(Debug, Clone)]
pub struct SkillDir {
    /// Skill name (the directory name under `skills/`).
    pub name: String,
    /// `(path-within-the-skill-dir, bytes)`.
    pub files: Vec<(String, Vec<u8>)>,
}

impl PluginComponents {
    /// Expand every [`AGENTGEAR_CLIENT_TOKEN`] to `client` across the surfaces that
    /// carry an executable command — hook commands and mcp command/args — so each
    /// backend renders its own canonical client id. Matchers, env, and markdown are
    /// left verbatim; a token-free plugin is unchanged.
    ///
    /// Public so an out-of-crate [`AgentBackend`](crate::AgentBackend) can expand the
    /// token after obtaining the IR through
    /// [`Plugin::components`](crate::host::Plugin::components), instead of shipping the
    /// literal token into a harness config or re-implementing the walk.
    pub fn with_client(mut self, client: &str) -> Self {
        for hook in &mut self.hooks {
            if hook.command.contains(AGENTGEAR_CLIENT_TOKEN) {
                hook.command = expand_client(&hook.command, client);
            }
        }
        for server in &mut self.mcp_servers {
            if server.command.contains(AGENTGEAR_CLIENT_TOKEN) {
                server.command = expand_client(&server.command, client);
            }
            for arg in &mut server.args {
                if arg.contains(AGENTGEAR_CLIENT_TOKEN) {
                    *arg = expand_client(arg, client);
                }
            }
        }
        self
    }

    /// Parse from flattened `(rel-path, bytes)` tree entries.
    pub(crate) fn parse(entries: &[(String, Vec<u8>)]) -> Result<Self> {
        let mut out = PluginComponents::default();
        let lookup = |rel: &str| entries.iter().find(|(r, _)| norm(r) == rel).map(|(_, b)| b.as_slice());

        parse_mcp(&lookup, &mut out.mcp_servers)?;

        for rel in ["hooks/hooks.json", ".claude-plugin/hooks.json"] {
            if let Some(bytes) = lookup(rel) {
                parse_hooks(rel, bytes, &mut out.hooks)?;
            }
        }

        for (rel, bytes) in entries {
            let n = norm(rel);
            if n.starts_with("commands/") && n.ends_with(".md") {
                out.commands.push(markdown_doc(&n, bytes));
            } else if n.starts_with("agents/") && n.ends_with(".md") {
                out.agents.push(markdown_doc(&n, bytes));
            }
        }

        parse_skills(entries, &mut out.skills);
        Ok(out)
    }
}

fn norm(rel: &str) -> String {
    rel.replace('\\', "/")
}

// --- mcp ---------------------------------------------------------------------

/// Gather servers from `plugin.json` `mcpServers` (object, or a string path to a
/// `.mcp.json`) plus a root/`.claude-plugin` `.mcp.json`. First occurrence of a
/// name wins so plugin.json takes precedence over a referenced file.
fn parse_mcp<'a>(lookup: &impl Fn(&str) -> Option<&'a [u8]>, out: &mut Vec<McpServer>) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();

    if let Some(bytes) = lookup(".claude-plugin/plugin.json") {
        let json = parse_json(".claude-plugin/plugin.json", bytes)?;
        match json.get("mcpServers") {
            Some(Value::Object(map)) => push_servers(map, &mut seen, out),
            // A string value is a path (relative to the tree) to a `.mcp.json`.
            Some(Value::String(path)) => {
                if let Some(bytes) = lookup(&norm(path)) {
                    let doc = parse_json(path, bytes)?;
                    if let Some(map) = mcp_object_in(&doc) {
                        push_servers(map, &mut seen, out);
                    }
                }
            }
            _ => {}
        }
    }

    for rel in [".mcp.json", ".claude-plugin/.mcp.json"] {
        if let Some(bytes) = lookup(rel) {
            let doc = parse_json(rel, bytes)?;
            if let Some(map) = mcp_object_in(&doc) {
                push_servers(map, &mut seen, out);
            }
        }
    }
    Ok(())
}

fn push_servers(map: &Map<String, Value>, seen: &mut std::collections::BTreeSet<String>, out: &mut Vec<McpServer>) {
    for (name, spec) in map {
        if seen.insert(name.clone())
            && let Some(server) = server_from_spec(name, spec)
        {
            out.push(server);
        }
    }
}

/// The server map inside a `.mcp.json` document: the standard `mcpServers` wrapper
/// when present, else the document's own root object (a bare `{name: spec}` map).
fn mcp_object_in(json: &Value) -> Option<&Map<String, Value>> {
    match json.get("mcpServers") {
        Some(v) => v.as_object(),
        None => json.as_object(),
    }
}

fn server_from_spec(name: &str, spec: &Value) -> Option<McpServer> {
    let obj = spec.as_object()?;
    let url = || obj.get("url").and_then(Value::as_str).unwrap_or_default().to_string();
    let kind = match obj.get("type").and_then(Value::as_str) {
        Some("http") => McpKind::Http { url: url() },
        Some("sse") => McpKind::Sse { url: url() },
        _ => McpKind::Stdio, // absent or "stdio"
    };
    let command = obj.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
    let args = obj
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let env = obj
        .get("env")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();
    Some(McpServer { name: name.to_string(), kind, command, args, env })
}

// --- hooks -------------------------------------------------------------------

fn parse_hooks(rel: &str, bytes: &[u8], out: &mut Vec<HookBinding>) -> Result<()> {
    let json = parse_json(rel, bytes)?;
    let Some(events) = json.get("hooks").and_then(Value::as_object) else {
        return Ok(());
    };
    for (event, groups) in events {
        let Some(groups) = groups.as_array() else { continue };
        for group in groups {
            let matcher = group.get("matcher").and_then(Value::as_str).map(str::to_string);
            let Some(handlers) = group.get("hooks").and_then(Value::as_array) else { continue };
            for handler in handlers {
                if let Some(command) = handler.get("command").and_then(Value::as_str) {
                    out.push(HookBinding { event: event.clone(), matcher: matcher.clone(), command: command.to_string() });
                }
            }
        }
    }
    Ok(())
}

// --- markdown docs -----------------------------------------------------------

fn markdown_doc(rel: &str, bytes: &[u8]) -> MarkdownDoc {
    let name = rel.rsplit('/').next().unwrap_or(rel).strip_suffix(".md").unwrap_or(rel).to_string();
    let text = String::from_utf8_lossy(bytes);
    let (frontmatter, body) = split_frontmatter(&text);
    MarkdownDoc { name, rel: rel.to_string(), frontmatter, body, raw: bytes.to_vec() }
}

/// Split a leading `---`-fenced frontmatter block. Parsed yaml-ish: flat
/// `key: value` lines, plus a literal/folded block scalar (`key: |`/`key: >`,
/// with an optional `-`/`+`/digit modifier) whose more-indented continuation
/// lines are joined back in (enough for CC command/agent headers). No fence ->
/// empty map + the full text as the body. Walks byte offsets in `rest` directly
/// (not a reconstructed `line.len() + 1` sum) so a `\r\n`-authored doc slices its
/// body at the true position instead of leaking the closing fence's bytes into it.
fn split_frontmatter(text: &str) -> (BTreeMap<String, Value>, String) {
    let rest = match text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) {
        Some(rest) => rest,
        None => return (BTreeMap::new(), text.to_string()),
    };
    let mut map = BTreeMap::new();
    let mut pos = 0usize;
    while pos < rest.len() {
        let nl = rest[pos..].find('\n').map(|i| pos + i);
        let line = rest[pos..nl.unwrap_or(rest.len())].trim_end_matches('\r');
        let mut next = nl.map_or(rest.len(), |i| i + 1);
        if line.trim() == "---" {
            return (map, rest.get(next..).unwrap_or_default().to_string());
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_string();
            let raw_value = v.trim();
            if let Some(strip_trailing_newline) = block_scalar_chomp(raw_value) {
                let (block, after) = read_block_scalar(rest, next, strip_trailing_newline);
                map.insert(key, Value::String(block));
                next = after;
            } else {
                let value = raw_value.trim_matches('"').trim_matches('\'');
                map.insert(key, Value::String(value.to_string()));
            }
        }
        pos = next;
    }
    // Unterminated fence: treat the whole thing as body, no frontmatter.
    (BTreeMap::new(), text.to_string())
}

/// `Some(strip)` iff `value` is a YAML block-scalar indicator: `|` (literal) or
/// `>` (folded — treated the same as `|` here, joined with `\n` rather than real
/// YAML folding; enough fidelity for a CC command/agent header), with an optional
/// explicit-indent digit and/or `-`/`+` chomp modifier. `strip` is true for `-`
/// (drop the trailing newline); the default/`+` (kept as one trailing newline)
/// collapse to `false` — a reasonable minimum, not real "keep" semantics.
fn block_scalar_chomp(value: &str) -> Option<bool> {
    let mut chars = value.chars();
    match chars.next()? {
        '|' | '>' => {}
        _ => return None,
    }
    let modifiers = chars.as_str();
    if modifiers.is_empty() {
        return Some(false);
    }
    modifiers.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '+').then(|| modifiers.contains('-'))
}

/// Collect a block scalar's continuation lines starting at byte offset `start` in
/// `rest`: every blank line, or line indented deeper than the `key:` line, joined
/// with `\n` and stripped of the block's own indent (the first non-blank line's).
/// Returns the joined value and the byte offset just past the last consumed line.
fn read_block_scalar(rest: &str, start: usize, strip_trailing_newline: bool) -> (String, usize) {
    let mut pos = start;
    let mut lines: Vec<&str> = Vec::new();
    while pos < rest.len() {
        let nl = rest[pos..].find('\n').map(|i| pos + i);
        let line = rest[pos..nl.unwrap_or(rest.len())].trim_end_matches('\r');
        let indent = line.len() - line.trim_start_matches(' ').len();
        if !line.trim().is_empty() && indent == 0 {
            break; // dedented back to (or past) the key line: block over.
        }
        lines.push(line);
        pos = nl.map_or(rest.len(), |i| i + 1);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return (String::new(), pos);
    }
    let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start_matches(' ').len()).min().unwrap_or(0);
    let joined = lines.iter().map(|l| l.get(indent.min(l.len())..).unwrap_or("")).collect::<Vec<_>>().join("\n");
    (if strip_trailing_newline { joined } else { format!("{joined}\n") }, pos)
}

// --- skills ------------------------------------------------------------------

fn parse_skills(entries: &[(String, Vec<u8>)], out: &mut Vec<SkillDir>) {
    let mut by_name: BTreeMap<String, Vec<(String, Vec<u8>)>> = BTreeMap::new();
    for (rel, bytes) in entries {
        let n = norm(rel);
        let Some(rest) = n.strip_prefix("skills/") else { continue };
        let Some((skill, within)) = rest.split_once('/') else { continue };
        by_name.entry(skill.to_string()).or_default().push((within.to_string(), bytes.clone()));
    }
    for (name, files) in by_name {
        out.push(SkillDir { name, files });
    }
}

// --- shared ------------------------------------------------------------------

fn parse_json(what: &str, bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|source| Error::Json { what: what.to_string(), source })
}

#[cfg(test)]
#[path = "../tests/unit/components.rs"]
mod components_tests;
