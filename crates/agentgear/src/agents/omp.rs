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
//! **Agent translation is retired when omp already surfaces the agents itself.** omp's
//! default-on `claude-plugins` provider walks CC's own on-disk registry (`$HOME/.claude/
//! plugins/installed_plugins.json`, HOME-based per `src/discovery/helpers.ts:895`) and
//! loads each listed plugin's agents straight off its `installPath` — so once the `claude`
//! backend has us registered there, our own agent files would register the same agents
//! twice (our `<plugin>-<stem>` namespacing guarantees distinct names that defeat omp's
//! exact-name dedup). `cc_registry_covers_agents` gates the agent write/probe on that,
//! retiring iff BOTH the plugin is listed in CC's `installed_plugins.json` AND omp's own
//! `claude-plugins` provider is enabled. The registry read itself
//! (`super::ccregistry::registry_lists_plugin`) is shared with the cursor backend's own
//! `loadClaude`-coverage gate. It is HOME-based (omp ignores `CLAUDE_CONFIG_DIR`, so a
//! relocated config dir moves the registry off this path and we translate). Provider-
//! enabled is omp's `isProviderEnabled` =
//! `!disabledProviders.has("claude-plugins")` (`src/capability/index.ts:289`),
//! `disabledProviders` loaded from omp's global `config.yml`, default empty = on. CC's own
//! enabled/disabled state is NOT consulted: omp surfaces a CC agent from
//! `installed_plugins.json` regardless of CC's `enabledPlugins` (verify-omp #2/#3), so the
//! provider toggle is the real signal. mcp + commands always translate. Either condition
//! false -> translate, so an omp-only install, a relocated `CLAUDE_CONFIG_DIR`, or a
//! user-disabled provider never loses its agents.
//!
//! Skipped surfaces (see `docs/harness/omp.md`): hooks (omp's only hook surface is
//! an in-process TS plugin API registering `pi.on(...)` — there is no config-writable
//! shell-hook file, same shape of gap as opencode), skills, and the CC `model` alias
//! on agents (`sonnet`/`opus`/`haiku` are not omp model ids; omp's default applies).
//! omp deliberately ignores cross-harness `.claude/.codex/.gemini` agent dirs
//! (`TASK_AGENT_CONFIG_SOURCE=".omp"`), so translation must land in omp's *own* dirs.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::ccregistry::registry_lists_plugin;
use super::confedit::{remove_file_idem, write_file_idem, yaml_scalar};
use super::mcpjson::{self, ServerShape};
use super::report;
use super::{AgentBackend, BackendState};
use crate::components::MarkdownDoc;
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
        // in-process TS plugin API (see the module doc). mcp + commands + agents
        // translate; the agent-retire on cc-registry coverage is a runtime gate, not
        // a capability absence. No skills surface.
        Capabilities {
            plugins: false,
            mcp: true,
            hooks: false,
            commands: true,
            agents: true,
            skills: false,
            instructions: false,
            scopes: &["user", "project"],
        }
    }

    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // Compose every surface (mcp + the command/agent markdown files), so a missing
        // command or agent file behind a healthy mcp.json reads NeedsRepair. `source`
        // is the one self_heal resolved for this agent (rehydrated `--path`, else the
        // compile-time default), so probe and reconcile render identical bytes.
        let comp = plugin.components(source)?;
        let base = surface_base(scope)?;
        let mcp = mcpjson::probe_surface(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::plain())?;
        let commands = report::probe_files(
            &expected_docs(&base.join("commands"), plugin.name, "commands/", &comp.commands, |doc| doc.raw.clone()),
            |_, _| true,
        )?;
        // Retired agents own nothing on disk, so the surface contributes `None` (not
        // `Absent`). Probing the never-written files would read `Absent` and, folded
        // behind a healthy mcp, churn the backend to `NeedsRepair` on every self_heal.
        let agents = if cc_registry_covers_agents(plugin) {
            None
        } else {
            report::probe_files(
                &expected_docs(&base.join("agents"), plugin.name, "agents/", &comp.agents, |doc| {
                    render_agent(plugin.name, doc).into_bytes()
                }),
                |_, _| true,
            )?
        };
        Ok(report::compose([mcp, commands, agents].into_iter().flatten()))
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&desired.source)?;
        let base = surface_base(scope)?;

        let mut changed = false;
        changed |= mcpjson::reconcile(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;

        // Commands are markdown + frontmatter in both CC and omp (omp reads
        // `frontmatter.description` or the first body line), so the verbatim bytes
        // are already a valid omp command; unknown CC frontmatter keys are ignored.
        let cmd_root = base.join("commands");
        for doc in &comp.commands {
            changed |= write_file_idem(&cmd_root.join(doc_file(plugin.name, &doc.rel, "commands/")), &doc.raw)?;
        }
        // Retire the agent translation when CC's own registry already surfaces these
        // agents to omp (the `claude` backend installed us); mcp + commands still land.
        if !cc_registry_covers_agents(plugin) {
            let agent_root = base.join("agents");
            for doc in &comp.agents {
                changed |= write_file_idem(
                    &agent_root.join(doc_file(plugin.name, &doc.rel, "agents/")),
                    render_agent(plugin.name, doc).as_bytes(),
                )?;
            }
        }
        Ok(if changed { Outcome::Installed } else { Outcome::NoOp })
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
        let comp = plugin.components(&Source::Embedded)?;
        let base = surface_base(scope)?;

        let mut changed = false;
        changed |= mcpjson::remove(&base.join("mcp.json"), &["mcpServers"], &comp.mcp_servers, ServerShape::plain())? != Outcome::NoOp;

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
    resolve_omp_root(config_dir_name().as_os_str(), dirs::home_dir().as_deref())
}

/// Join a config-dir name onto `home` the way omp does: node's
/// `path.join(os.homedir(), name)` *appends* even an absolute `name`, where Rust's
/// `Path::join` would instead *replace* the base (verified live,
/// `docs/harness/omp.md` gotcha 1). Any root component (`/`, a Windows drive
/// prefix, `\\`) is stripped from `name` first so the join always appends, matching
/// node. Split from the env/home lookup so a unit test can exercise it without
/// mutating process-global env.
fn resolve_omp_root(name: &OsStr, home: Option<&Path>) -> Option<PathBuf> {
    let relative: PathBuf = Path::new(name).components().filter(|c| !matches!(c, Component::RootDir | Component::Prefix(_))).collect();
    home.map(|h| h.join(relative))
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

/// The `(path, rendered bytes)` files `probe` compares against disk for a surface
/// dir, keyed off the same `doc_file` + render `reconcile` writes.
fn expected_docs(
    dir: &Path, plugin: &str, prefix: &str, docs: &[MarkdownDoc], render: impl Fn(&MarkdownDoc) -> Vec<u8>,
) -> Vec<(PathBuf, Vec<u8>)> {
    docs.iter().map(|doc| (dir.join(doc_file(plugin, &doc.rel, prefix)), render(doc))).collect()
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

// --- cc-registry retire gate -------------------------------------------------

/// omp's `claude-plugins` provider id — the one whose enabled state gates the retire.
const CLAUDE_PLUGINS_PROVIDER: &str = "claude-plugins";

/// True when omp's `claude-plugins` provider already surfaces this plugin's agents off
/// Claude Code's on-disk registry, so translating them ourselves would double-register.
/// Retires iff the plugin is listed in CC's registry AND omp's provider is enabled — see
/// the module doc for why CC's own enabled state is deliberately not part of the signal.
fn cc_registry_covers_agents(plugin: &Plugin) -> bool {
    let Some(cc) = dirs::home_dir().map(|h| h.join(".claude")) else {
        return false;
    };
    registry_lists_plugin(&cc.join("plugins").join("installed_plugins.json"), &plugin.id()) && omp_claude_plugins_provider_enabled()
}

/// omp's `isProviderEnabled("claude-plugins")` (`src/capability/index.ts:289`) resolved
/// from disk: `!disabledProviders.has("claude-plugins")`, where `disabledProviders` comes
/// from omp's global config `~/.omp/agent/config.{yml,yaml}` (default empty = enabled).
/// `omp_root` honors `PI_CONFIG_DIR` like the rest of the backend. An absent/unreadable
/// config reads as enabled (the provider is on by default, PRIORITY 70).
fn omp_claude_plugins_provider_enabled() -> bool {
    let Some(agent) = omp_root().map(|r| r.join("agent")) else {
        return true;
    };
    // MAIN_CONFIG_FILENAMES: `config.yml` is primary, `config.yaml` the alternate.
    !["config.yml", "config.yaml"].iter().any(|name| config_disables_claude_plugins(&agent.join(name)))
}

/// True when omp's config at `path` disables the `claude-plugins` provider.
/// `disabledProviders` is a YAML sequence of either plain provider-id strings or
/// path-scoped `{path…, providers/values/items: […]}` objects (settings.ts
/// `resolvePathScopedStringArray`). We match the id against every string leaf under the
/// key — covering both forms — and ignore the path scope, so ANY disable directive
/// naming the provider reads as disabled. That is the conservative side (translate rather
/// than retire), so a disabled provider never loses its agents. Absent file/key -> false.
fn config_disables_claude_plugins(path: &Path) -> bool {
    let Some(root) = fs::read(path).ok().and_then(|b| serde_norway::from_slice::<serde_norway::Value>(&b).ok()) else {
        return false;
    };
    root.get("disabledProviders").is_some_and(|v| yaml_has_string_leaf(v, CLAUDE_PLUGINS_PROVIDER))
}

/// Recursively true when any string leaf in `value` (sequences + mapping values walked)
/// equals `needle` — matches a plain `disabledProviders` string and a path-scoped entry's
/// nested `providers`/`values`/`items` list without modeling the object shape.
fn yaml_has_string_leaf(value: &serde_norway::Value, needle: &str) -> bool {
    use serde_norway::Value as Yaml;
    match value {
        Yaml::String(s) => s == needle,
        Yaml::Sequence(seq) => seq.iter().any(|v| yaml_has_string_leaf(v, needle)),
        Yaml::Mapping(map) => map.iter().any(|(_, v)| yaml_has_string_leaf(v, needle)),
        _ => false,
    }
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

    let root = report::read_json_config(&mut checks, "mcp.json", &mcp);

    let Some(comp) = report::components(&mut checks, plugin, source) else {
        return checks;
    };

    checks.push(report::check_mcp_registered(
        &comp.mcp_servers,
        root.as_ref(),
        &["mcpServers"],
        "not in mcp.json",
        "run the host's `setup`",
    ));
    checks.push(report::check_mcp_command(&comp.mcp_servers));
    checks.push(check_docs_present("translated commands present", &comp.commands, &base.join("commands"), plugin.name, "commands/"));
    // When CC's registry covers the agents, we deliberately write none, so file-existence
    // is not a health signal — report the retire rather than a spurious "missing" Fail.
    checks.push(if cc_registry_covers_agents(plugin) {
        DoctorCheck {
            name: "translated agents present",
            status: CheckStatus::Ok("covered by Claude Code's plugin registry; not translated".into()),
        }
    } else {
        check_docs_present("translated agents present", &comp.agents, &base.join("agents"), plugin.name, "agents/")
    });

    checks
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
