//! The omp (oh-my-pi) backend: a full translate into omp's own OMP-native config
//! tree. MCP goes through the shared json renderer into `mcpServers` of
//! `~/.omp/agent/mcp.json` (user) or `<cwd>/.omp/mcp.json` (project), Plain shape
//! (`{command,args,env}`) — omp's stdio schema defaults `type` to `stdio`, so the
//! minimal Plain body is a valid entry. CC commands copy through verbatim as
//! markdown (omp reads `frontmatter.description` or the first body line, so the CC
//! command file is already a valid omp command); CC agent defs re-emit as omp task
//! agents (`agents/<plugin>-<stem>.md`) — omp reads `name`/`description` from
//! frontmatter and takes the markdown body as `systemPrompt`, the exact shape a CC
//! agent already has. Every file is plugin-name-prefixed and every mcp key is our
//! own server name, so `remove` is exact and a second reconcile is a true `NoOp`.
//!
//! Skipped surfaces (see `docs/harness/omp.md`): hooks (omp's only hook surface is
//! an in-process TS plugin API registering `pi.on(...)` — there is no config-writable
//! shell-hook file, same shape of gap as opencode), skills, and the CC `model` alias
//! on agents (`sonnet`/`opus`/`haiku` are not omp model ids; omp's default applies).
//! omp deliberately ignores cross-harness `.claude/.codex/.gemini` agent dirs
//! (`TASK_AGENT_CONFIG_SOURCE=".omp"`), so translation must land in omp's *own* dirs.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::confedit::{remove_file_idem, write_file_idem};
use super::mcpjson::{self, ServerShape};
use super::{AgentBackend, BackendState};
use crate::components::{MarkdownDoc, McpKind, McpServer};
use crate::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};

pub(crate) struct OmpBackend;

impl AgentBackend for OmpBackend {
    fn id(&self) -> &'static str {
        "omp"
    }

    fn detect(&self) -> bool {
        // `~/.omp` is HOME-based (relocatable only via the `PI_CONFIG_DIR` name
        // override, honored by `omp_root`), so a test redirecting `HOME` redirects
        // detection too; the `omp` binary on PATH is the direct signal.
        which::which("omp").is_ok() || omp_root().is_some_and(|r| r.is_dir())
    }

    fn capabilities(&self) -> Capabilities {
        // `hooks:false` — omp has no declarative shell-hook config, only an
        // in-process TS plugin API (see the module doc).
        Capabilities { plugins: false, mcp: true, hooks: false, scopes: &["user", "project"] }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        // Ownership is defined by our mcp server keys (the canonical "are we here"
        // signal); the shared probe returns Healthy — never Absent — for an mcp-less
        // plugin, so a present marker is never dropped. Source::Embedded is the only
        // steady-state source for a non-CC backend (github unsupported, path is
        // install-only), mirroring the claude probe keying on compile-time metadata.
        let comp = plugin.components(&Source::Embedded)?;
        mcpjson::probe(&mcp_path(scope)?, &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = surface_base(scope)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::Plain)? != Outcome::NoOp;

        // Commands are markdown + frontmatter in both CC and omp (omp reads
        // `frontmatter.description` or the first body line), so the verbatim bytes
        // are already a valid omp command; unknown CC frontmatter keys are ignored.
        let cmd_root = base.join("commands");
        for doc in &comp.commands {
            changed |= write_file_idem(&cmd_root.join(doc_file(plugin.name, &doc.rel, "commands/")), &doc.raw)?;
        }
        let agent_root = base.join("agents");
        for doc in &comp.agents {
            changed |=
                write_file_idem(&agent_root.join(doc_file(plugin.name, &doc.rel, "agents/")), render_agent(plugin.name, doc).as_bytes())?;
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let base = surface_base(scope)?;

        let mut changed = false;
        changed |= mcpjson::remove(&base.join("mcp.json"), &["mcpServers"], &portable_names(&comp.mcp_servers))? != Outcome::NoOp;

        // `commands/` and `agents/` are shared with the user's own files (omp scans
        // them flat), so we delete only our plugin-prefixed files by name — never a
        // `remove_dir_all`.
        let cmd_root = base.join("commands");
        for doc in &comp.commands {
            changed |= remove_file_idem(&cmd_root.join(doc_file(plugin.name, &doc.rel, "commands/")))?;
        }
        let agent_root = base.join("agents");
        for doc in &comp.agents {
            changed |= remove_file_idem(&agent_root.join(doc_file(plugin.name, &doc.rel, "agents/")))?;
        }
        Ok(if changed { Outcome::Removed } else { Outcome::NoOp })
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        DoctorReport::from_checks(report_checks(self, plugin, source))
    }
}

// --- paths -------------------------------------------------------------------

/// The config-dir name under HOME: `PI_CONFIG_DIR` when set (omp's documented
/// override, read as a name relative to home per `getConfigDirName`), else `.omp`.
fn config_dir_name() -> OsString {
    std::env::var_os("PI_CONFIG_DIR").filter(|v| !v.is_empty()).unwrap_or_else(|| ".omp".into())
}

/// The omp config root `~/.omp` (or `~/<PI_CONFIG_DIR>`). `None` when HOME is unset.
fn omp_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(config_dir_name()))
}

/// The surface base holding `mcp.json` + `commands/` + `agents/` for a scope:
/// `~/.omp/agent` (user — note the extra `agent/` segment omp's user scope adds) or
/// `<cwd>/.omp` (project). Project scope always uses the fixed `.omp` name (omp keys
/// project dirs to cwd via `CONFIG_DIR_NAME`, unaffected by the home-relative
/// `PI_CONFIG_DIR`). A missing HOME at user scope is a clear, actionable error.
fn surface_base(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => omp_root()
            .map(|r| r.join("agent"))
            .ok_or_else(|| Error::Tree("no home directory (HOME unset); cannot locate ~/.omp/agent".into())),
        Scope::Project { path } => Ok(path.join(".omp")),
    }
}

fn mcp_path(scope: &Scope) -> Result<PathBuf> {
    Ok(surface_base(scope)?.join("mcp.json"))
}

/// Server names `reconcile` actually writes (the shared renderer skips non-portable
/// ones). `remove` keys off the same set so an unfiltered name list can never delete
/// a user server sharing a name with one we declared but never wrote (e.g. a
/// `${CLAUDE_PLUGIN_ROOT}`-bearing entry).
fn portable_names(servers: &[McpServer]) -> Vec<&str> {
    servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect()
}

// --- commands / agents -------------------------------------------------------

/// `commands/hello.md` -> `<plugin>-hello.md`; a nested path flattens (`a/b.md` ->
/// `<plugin>-a-b.md`). omp scans `commands/*.md` / `agents/*.md` flat (non-recursive),
/// so nesting is flattened; the plugin prefix keeps the file identifiably ours for an
/// exact `remove` and clear of an omp bundled agent name (`scout`/`reviewer`/…).
fn doc_file(plugin: &str, rel: &str, prefix: &str) -> String {
    format!("{plugin}-{}.md", flat_stem(rel, prefix))
}

fn flat_stem(rel: &str, prefix: &str) -> String {
    let stripped = rel.strip_prefix(prefix).unwrap_or(rel);
    let stem = stripped.strip_suffix(".md").unwrap_or(stripped);
    stem.replace(['/', '\\'], "-")
}

/// Render a CC agent def as an omp task agent. omp normalizes `name`/`description`
/// from frontmatter (both required for a valid agent) and takes the markdown body as
/// `systemPrompt` — the identical shape of a CC agent, so this is a re-emit with an
/// ownership-safe namespaced `name` (omp dedups by exact name, first-wins, against
/// bundled + user agents). The CC `model` alias is dropped: `sonnet`/`opus`/`haiku`
/// are not omp model ids, so omp's own default applies. Deterministic so a
/// re-reconcile is byte-identical (a true `NoOp`).
fn render_agent(plugin: &str, doc: &MarkdownDoc) -> String {
    let mut out = String::from("---\n");
    let _ = writeln!(out, "name: {}", yaml_scalar(&format!("{plugin}-{}", flat_stem(&doc.rel, "agents/"))));
    if let Some(desc) = doc.frontmatter.get("description").and_then(Value::as_str) {
        let _ = writeln!(out, "description: {}", yaml_scalar(desc));
    }
    out.push_str("---\n\n");
    out.push_str(doc.body.trim());
    out.push('\n');
    out
}

/// A YAML scalar: bare when it cannot be misparsed as a flow/indicator token, else a
/// double-quoted string with the minimal escapes, so an arbitrary name/description
/// stays a single safe scalar regardless of colons or quotes.
fn yaml_scalar(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.starts_with(|c: char| c.is_ascii_whitespace())
        || s.ends_with(|c: char| c.is_ascii_whitespace())
        || s.contains(['"', '\\', '\n', '\r', '\t', ':', '#', '[', ']', '{', '}', ',', '&', '*', '!', '|', '>', '\'', '%', '@', '`'])
        || matches!(s.to_ascii_lowercase().as_str(), "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~");
    if !needs_quote {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// --- report ------------------------------------------------------------------

fn report_checks(backend: &OmpBackend, plugin: &Plugin, source: &Source) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if backend.detect() {
        DoctorCheck { name: "omp detected", status: CheckStatus::Ok("`omp` on PATH or ~/.omp present".into()) }
    } else {
        DoctorCheck {
            name: "omp detected",
            status: CheckStatus::Fail {
                problem: "omp not detected".into(),
                fix: "install it with `curl -fsSL https://omp.sh/install | sh`".into(),
            },
        }
    });

    let base = match surface_base(&Scope::User) {
        Ok(base) => base,
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(e.to_string()) });
            return checks;
        }
    };
    let mcp = base.join("mcp.json");

    let root = match fs::read(&mcp) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Ok(format!("{} parses", mcp.display())) });
                Some(v)
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    name: "mcp.json",
                    status: CheckStatus::Fail {
                        problem: format!("{} does not parse: {e}", mcp.display()),
                        fix: "fix the JSON syntax or remove the file".into(),
                    },
                });
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(DoctorCheck {
                name: "mcp.json",
                status: CheckStatus::Warn(format!("{} does not exist yet (run setup)", mcp.display())),
            });
            None
        }
        Err(e) => {
            checks.push(DoctorCheck { name: "mcp.json", status: CheckStatus::Warn(format!("could not read {}: {e}", mcp.display())) });
            None
        }
    };

    let comp = match plugin.components(source) {
        Ok(comp) => comp,
        Err(e) => {
            checks.push(DoctorCheck {
                name: "plugin components",
                status: CheckStatus::Fail {
                    problem: format!("could not read the plugin tree: {e}"),
                    fix: "rebuild the host binary".into(),
                },
            });
            return checks;
        }
    };

    checks.push(check_mcp_registered(&comp.mcp_servers, root.as_ref()));
    checks.push(check_mcp_command(&comp.mcp_servers));
    checks.push(check_docs_present("translated commands present", &comp.commands, &base.join("commands"), plugin.name, "commands/"));
    checks.push(check_docs_present("translated agents present", &comp.agents, &base.join("agents"), plugin.name, "agents/"));

    checks
}

fn check_mcp_registered(servers: &[McpServer], root: Option<&Value>) -> DoctorCheck {
    let name = "mcp server registered";
    let portable: Vec<&str> = servers.iter().filter(|s| s.is_portable()).map(|s| s.name.as_str()).collect();
    if portable.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("no portable mcp servers to register".into()) };
    }
    let obj = root.and_then(|r| r.get("mcpServers")).and_then(Value::as_object);
    let missing: Vec<&str> = portable.iter().copied().filter(|n| obj.is_none_or(|o| !o.contains_key(*n))).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} registered", portable.join(", "))) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp server(s) not in mcp.json: {}", missing.join(", ")),
                fix: "run the host's `setup`".into(),
            },
        }
    }
}

fn check_mcp_command(servers: &[McpServer]) -> DoctorCheck {
    let name = "mcp command on PATH";
    let missing: Vec<String> = servers
        .iter()
        .filter(|s| s.is_portable() && matches!(s.kind, McpKind::Stdio))
        .map(|s| s.command.clone())
        // Only a bare executable name is a PATH lookup; a path/variable command can't be checked generically.
        .filter(|c| !c.is_empty() && !c.contains('/') && !c.contains('\\') && !c.contains('$') && which::which(c).is_err())
        .collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok("all referenced mcp commands resolve".into()) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail {
                problem: format!("mcp command(s) not on PATH: {}", missing.join(", ")),
                fix: "install the missing binaries into a PATH directory".into(),
            },
        }
    }
}

fn check_docs_present(name: &'static str, docs: &[MarkdownDoc], dir: &Path, plugin: &str, prefix: &str) -> DoctorCheck {
    if docs.is_empty() {
        return DoctorCheck { name, status: CheckStatus::Ok("nothing to translate".into()) };
    }
    let missing: Vec<String> = docs.iter().map(|d| doc_file(plugin, &d.rel, prefix)).filter(|f| !dir.join(f).exists()).collect();
    if missing.is_empty() {
        DoctorCheck { name, status: CheckStatus::Ok(format!("{} file(s) present", docs.len())) }
    } else {
        DoctorCheck {
            name,
            status: CheckStatus::Fail { problem: format!("file(s) missing: {}", missing.join(", ")), fix: "run the host's `setup`".into() },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/omp.rs"]
mod omp_tests;
