//! The public surface a host binary drives: the [`PluginHost`] trait (the derive
//! implements it) plus the value types its lifecycle methods speak in.

use std::path::{Path, PathBuf};

use crate::doctor::DoctorReport;
use crate::error::Result;

/// Where a plugin is installed. `Local` is intentionally absent (design §API):
/// binary-driven install of user-wide tooling has no coherent local-scope story.
/// `non_exhaustive` so a future variant lands without a semver major.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Scope {
    /// User-wide install (CC's `--scope user`).
    User,
    /// Install into one project's settings; the CLI keys off its working directory,
    /// so calls run with cwd set to `path`.
    Project {
        /// The project directory.
        path: PathBuf,
    },
}

impl Scope {
    /// The `--scope` value CC expects.
    pub(crate) fn as_cli(&self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project { .. } => "project",
        }
    }

    /// A project scope targets CC's project settings for a given directory; the
    /// CLI resolves that from its working directory, so calls run with cwd here.
    pub(crate) fn cwd(&self) -> Option<&Path> {
        match self {
            Scope::User => None,
            Scope::Project { path } => Some(path),
        }
    }

    /// Stable key fragment for the stamp marker hash. A project path is
    /// canonicalized first so the same project reached via a symlink and via its
    /// realpath key the same marker instead of double-installing; falls back to
    /// the raw path when canonicalize fails (e.g. the project dir doesn't exist
    /// yet).
    pub(crate) fn key(&self) -> String {
        match self {
            Scope::User => "user".to_string(),
            Scope::Project { path } => {
                let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                format!("project:{}", resolved.display())
            }
        }
    }
}

/// Runtime origin of the plugin tree, defaulted from the derive attrs but
/// overridable per call. One binary can ship an embedded tree yet still let users
/// track a GitHub ref so `claude plugin update` pulls new plugin versions without
/// waiting on a binary release.
///
/// Not `Copy`: [`Source::Path`] carries a `PathBuf`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The compile-time blob baked into the binary (needs the `embed` feature +
    /// derive attr). Decompressed and materialized locally.
    Embedded,
    /// A GitHub-hosted marketplace the `claude` CLI fetches directly, letting users
    /// track a ref for plugin updates without a new binary release. Non-plugin-native
    /// backends have no local tree here, so they skip (design §API).
    GitHub {
        /// `"owner/repo"`.
        repo: &'static str,
        /// The tracked git ref (the derive uses `v<version>`).
        ref_: &'static str,
    },
    /// An on-disk plugin tree (a dir holding `.claude-plugin/plugin.json`),
    /// materialized like [`Source::Embedded`] but read from `path` at runtime. Lets
    /// a `default-features = false` host with no baked blob still install.
    Path(PathBuf),
}

/// What a reconcile should converge to. Held separately from [`Scope`] because a
/// backend converges the same desired state across scopes.
#[derive(Debug, Clone)]
pub struct Desired {
    /// Where the plugin tree comes from for this reconcile.
    pub source: Source,
    /// `true` for an explicit `install`/`update` (the design says install flips
    /// enable state), `false` for self_heal/adopt (never re-enable a deliberate
    /// disable).
    pub reenable: bool,
}

/// What a lifecycle op actually did. `non_exhaustive`: adding a case is additive.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Already converged; nothing changed.
    NoOp,
    /// A fresh install landed.
    Installed,
    /// The install moved to a new version.
    Updated {
        /// The prior version, when it could be read.
        from: Option<String>,
        /// The version now installed.
        to: String,
    },
    /// A broken/partial state was reconciled back to healthy.
    Repaired,
    /// self_heal found a healthy install with no marker and wrote one.
    Adopted,
    /// The install was removed.
    Removed,
    /// self_heal found a cleanly-uninstalled plugin and cleared the stale marker.
    Cleared,
}

/// End-user wording (`installed`, `updated (0.1.0 -> 0.2.0)`), so a host can
/// print an outcome in a `setup` summary instead of exposing `{:?}`.
impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::NoOp => f.write_str("no changes needed"),
            Outcome::Installed => f.write_str("installed"),
            Outcome::Updated { from: Some(from), to } => write!(f, "updated ({from} -> {to})"),
            Outcome::Updated { from: None, to } => write!(f, "updated (to {to})"),
            Outcome::Repaired => f.write_str("repaired"),
            Outcome::Adopted => f.write_str("adopted existing install"),
            Outcome::Removed => f.write_str("removed"),
            Outcome::Cleared => f.write_str("cleared stale marker"),
        }
    }
}

/// Per-agent results of one lifecycle fan-out, one entry per configured agent in
/// `plugin.agents` order (agents excluded by an explicit `install_into` filter get
/// no entry — they were never asked for). The merged-[`Outcome`] lifecycle methods
/// collapse this to first-change-wins; a host that wants to tell its user which
/// agents were installed, skipped, or failed reads the `*_report` variants and
/// prints this (its `Display` is a ready `setup` summary, one line per agent).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AgentReport {
    /// One entry per configured agent that was asked to run, in `plugin.agents` order.
    pub results: Vec<AgentResult>,
}

/// One agent's slice of a lifecycle fan-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResult {
    /// The backend id (`"claude"`, `"codex"`, …).
    pub agent: &'static str,
    /// What that backend did.
    pub status: AgentStatus,
}

/// What one agent's slice of the fan-out did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentStatus {
    /// The backend ran; this is its own outcome (not the merged one).
    Converged(Outcome),
    /// The backend was skipped before it could write anything.
    Skipped(SkipReason),
    /// The backend failed, rendered for the user. The fan-out continued past it,
    /// so sibling entries still reflect real per-agent results.
    Failed(String),
}

/// Why an agent was skipped rather than converged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// `detect()` returned false — the tool is not on this machine.
    NotDetected,
    /// The backend has no config surface at the requested scope.
    ScopeUnsupported,
    /// The resolved source cannot serve this backend (a GitHub source needs a
    /// plugin-native backend; config-merge backends have no local tree to render).
    SourceUnsupported,
}

impl AgentReport {
    pub(crate) fn new() -> Self {
        Self { results: Vec::new() }
    }

    pub(crate) fn push(&mut self, agent: &'static str, status: AgentStatus) {
        self.results.push(AgentResult { agent, status });
    }

    /// The first real change across the agents (a change outranks a no-op);
    /// [`Outcome::NoOp`] when nothing changed. This is exactly what the merged
    /// lifecycle methods ([`PluginHost::install`], …) return on success.
    pub fn merged(&self) -> Outcome {
        self.results
            .iter()
            .find_map(|result| match &result.status {
                AgentStatus::Converged(outcome) if *outcome != Outcome::NoOp => Some(outcome.clone()),
                _ => None,
            })
            .unwrap_or(Outcome::NoOp)
    }

    /// True when no agent failed (skips are not failures).
    pub fn is_healthy(&self) -> bool {
        !self.results.iter().any(|result| matches!(result.status, AgentStatus::Failed(_)))
    }

    /// The legacy single-`Outcome` collapse: every agent already ran, so this is
    /// fail-at-end — the first failed agent decides the `Err` (as
    /// [`Error::Backend`](crate::Error::Backend)), else the merged outcome.
    pub(crate) fn into_merged(self) -> Result<Outcome> {
        for result in &self.results {
            if let AgentStatus::Failed(detail) = &result.status {
                return Err(crate::error::Error::Backend { agent: result.agent.into(), detail: detail.clone() });
            }
        }
        Ok(self.merged())
    }

    /// The `Converged` outcome of one agent, if it ran.
    pub(crate) fn outcome_of(&self, agent: &str) -> Option<&Outcome> {
        self.results.iter().find_map(|result| match &result.status {
            AgentStatus::Converged(outcome) if result.agent == agent => Some(outcome),
            _ => None,
        })
    }
}

impl std::fmt::Display for AgentReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for result in &self.results {
            writeln!(f, "{}: {}", result.agent, result.status)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Converged(outcome) => outcome.fmt(f),
            AgentStatus::Skipped(reason) => write!(f, "skipped ({reason})"),
            AgentStatus::Failed(detail) => write!(f, "failed: {detail}"),
        }
    }
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::NotDetected => f.write_str("not installed on this machine"),
            SkipReason::ScopeUnsupported => f.write_str("no config surface at this scope"),
            SkipReason::SourceUnsupported => f.write_str("cannot serve a github source; use an embedded or path source"),
        }
    }
}

/// What an agent backend can host. Each surface flag is `true` iff the backend's
/// `reconcile` actually writes/manages that surface for a plugin declaring it
/// (a conditionally-gated surface — e.g. one a native registry may already cover —
/// still counts, since the backend *can* translate it). Lets `setup` report
/// "codex: mcp only" instead of silently dropping features, and is the runtime
/// truth behind the README's supported-agents matrix.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Native plugin install (copies the whole CC tree); implies every surface
    /// except `instructions`, which is a non-CC context-file surface (a CC host
    /// delivers its guidance through the MCP `instructions` channel, not a file).
    pub plugins: bool,
    /// Manages MCP servers.
    pub mcp: bool,
    /// Manages hook bindings.
    pub hooks: bool,
    /// Manages slash commands.
    pub commands: bool,
    /// Manages subagent definitions.
    pub agents: bool,
    /// Manages skill directories.
    pub skills: bool,
    /// Host-authored always-loaded guidance written to the harness's native
    /// context channel (a dedicated instructions file + any registration).
    pub instructions: bool,
    /// The scope ids this backend supports (e.g. `["user", "project"]`).
    pub scopes: &'static [&'static str],
}

/// A resolved plugin descriptor. Built by [`PluginHost::descriptor`] from the
/// derive-emitted metadata; passed to backends.
#[derive(Clone)]
pub struct Plugin {
    /// Plugin name (`plugin.json`'s `name`).
    pub name: &'static str,
    /// Marketplace id in `<name>@<marketplace>`.
    pub marketplace: &'static str,
    /// The plugin version.
    pub version: &'static str,
    /// The configured backend ids this plugin fans out to.
    pub agents: &'static [&'static str],
    /// Host-authored always-loaded guidance ([`PluginHost::instructions`]); each
    /// non-CC backend writes it to its native context channel. `None` writes nothing.
    pub instructions: Option<String>,
    /// The plugin tree baked in as a compressed `.tar.br` (empty when the derive's
    /// `embed` attr is off). Decompressed by `materialize` for [`Source::Embedded`].
    pub(crate) blob: &'static [u8],
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("name", &self.name)
            .field("marketplace", &self.marketplace)
            .field("version", &self.version)
            .field("agents", &self.agents)
            .field("instructions", &self.instructions)
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// `<name>@<marketplace>`, the id every `claude plugin` call uses.
    pub fn id(&self) -> String {
        format!("{}@{}", self.name, self.marketplace)
    }

    pub(crate) fn blob(&self) -> &'static [u8] {
        self.blob
    }

    /// The harness-agnostic components IR for this plugin's tree. Public so an
    /// external [`AgentBackend`](crate::AgentBackend) can render from the same parsed
    /// IR the in-crate backends use instead of re-parsing the tree by hand.
    /// `Source::GitHub` has no local tree and errors out (non-CC backends cannot
    /// serve a github-source host).
    pub fn components(&self, source: &Source) -> Result<crate::components::PluginComponents> {
        crate::components::PluginComponents::parse(&crate::materialize::entries_for(self, source)?)
    }
}

/// Implemented by the `#[derive(PluginHost)]` macro. The consts carry the
/// compile-time metadata; the provided methods are the lifecycle the host calls.
pub trait PluginHost {
    /// Plugin name; must equal `plugin.json`'s `name` (the derive checks it).
    const NAME: &'static str;
    /// Marketplace id in `<name>@<marketplace>` (defaults to [`NAME`](Self::NAME)).
    const MARKETPLACE: &'static str;
    /// Plugin version; the host's `build.rs` pins it to `plugin.json`'s `version`.
    const VERSION: &'static str;
    /// The source a no-argument lifecycle call (`update`/`uninstall`/`self_heal`/`doctor`) uses.
    const DEFAULT_SOURCE: Source;
    /// The backend ids this host fans out to (the derive's `agents` list).
    const AGENTS: &'static [&'static str];

    /// The plugin tree baked into the host crate as a compressed `.tar.br` blob
    /// (the derive's `include_bytes!`). Empty when the derive's `embed` attr is
    /// off; [`Source::Embedded`] then errors at materialize.
    fn embedded_blob() -> &'static [u8];

    /// Host-authored always-loaded guidance merged into each non-CC harness's native
    /// instructions channel. `None` (the default) writes no instructions surface. A
    /// deriving host supplies it with `#[plugin(instructions_fn = <path>)]`, since the
    /// derive owns the sole `impl PluginHost` block and this is the only override seam.
    fn instructions() -> Option<String> {
        None
    }

    /// The resolved [`Plugin`] descriptor built from this host's consts, passed to
    /// the backends. Rarely overridden.
    fn descriptor() -> Plugin {
        Plugin {
            name: Self::NAME,
            marketplace: Self::MARKETPLACE,
            version: Self::VERSION,
            agents: Self::AGENTS,
            instructions: Self::instructions(),
            blob: Self::embedded_blob(),
        }
    }

    /// Idempotent: ensure the plugin is installed at the embedded version.
    /// Collapses [`PluginHost::install_report`] to one merged [`Outcome`]
    /// (first real change wins); a failed agent surfaces as `Err` after the
    /// whole fan-out ran.
    fn install(scope: Scope, source: Source) -> Result<Outcome> {
        Self::install_report(scope, source)?.into_merged()
    }

    /// [`PluginHost::install`] with per-agent results: which agents converged
    /// (and how), which were skipped (and why), which failed. `Err` only on a
    /// fatal precondition — the shared lock, or no usable data root (`HOME` and
    /// `XDG_DATA_HOME` both unset, so no agent could stamp a marker); per-agent
    /// failures live in the report so one bad agent never hides the rest.
    fn install_report(scope: Scope, source: Source) -> Result<AgentReport> {
        crate::install::install_report(&Self::descriptor(), scope, source, &[])
    }

    /// Like [`PluginHost::install`] but only into the `AGENTS` whose id is in
    /// `agents` (an empty slice = all of `AGENTS`). Lets a host target one backend
    /// (`setup --agent gemini`) without touching the others.
    fn install_into(scope: Scope, source: Source, agents: &[&str]) -> Result<Outcome> {
        Self::install_into_report(scope, source, agents)?.into_merged()
    }

    /// [`PluginHost::install_into`] with per-agent results; filtered-out agents
    /// get no entry.
    fn install_into_report(scope: Scope, source: Source, agents: &[&str]) -> Result<AgentReport> {
        crate::install::install_report(&Self::descriptor(), scope, source, agents)
    }

    /// Materialize a new versioned tree, then update the marketplace + plugin.
    /// Collapses [`PluginHost::update_report`] like [`PluginHost::install`].
    fn update(scope: Scope) -> Result<Outcome> {
        Self::update_report(scope)?.into_merged()
    }

    /// [`PluginHost::update`] with per-agent results. Same `Err` contract as
    /// [`PluginHost::install_report`]: only the lock or a missing data root.
    fn update_report(scope: Scope) -> Result<AgentReport> {
        crate::install::update_report(&Self::descriptor(), scope, Self::DEFAULT_SOURCE)
    }

    /// Uninstall, then refcount-gated marketplace remove; clears the marker.
    /// Collapses [`PluginHost::uninstall_report`] like [`PluginHost::install`].
    fn uninstall(scope: Scope) -> Result<Outcome> {
        Self::uninstall_report(scope)?.into_merged()
    }

    /// [`PluginHost::uninstall`] with per-agent results. Same `Err` contract as
    /// [`PluginHost::install_report`]: only the lock or a missing data root.
    fn uninstall_report(scope: Scope) -> Result<AgentReport> {
        crate::install::uninstall_report(&Self::descriptor(), scope, Self::DEFAULT_SOURCE)
    }

    /// SessionStart entrypoint. Repairs broken installs, never resurrects a
    /// deliberate uninstall, never downgrades, never re-enables (design §6).
    /// Collapses [`PluginHost::self_heal_report`] like [`PluginHost::install`].
    fn self_heal() -> Result<Outcome> {
        Self::self_heal_report()?.into_merged()
    }

    /// [`PluginHost::self_heal`] with per-agent results. Same `Err` contract as
    /// [`PluginHost::install_report`]: only the lock or a missing data root.
    fn self_heal_report() -> Result<AgentReport> {
        crate::selfheal::self_heal_report(&Self::descriptor(), Self::DEFAULT_SOURCE)
    }

    /// `Some(message)` when an update landed that the running CC session has not
    /// loaded yet (CC reads plugin contents at session start, no mid-session
    /// hot-reload). A `UserPromptSubmit` hook in the plugin tree calls a host
    /// `check-restart` subcommand that prints this; the model then asks the user to
    /// restart Claude Code. Disk errors collapse to `None`: a per-prompt hook must
    /// stay benign, and a real disk failure surfaces through the mutate paths.
    fn restart_pending() -> Option<String> {
        let plugin = Self::descriptor();
        crate::restart::pending(&plugin).ok().flatten().map(|()| crate::restart::message(Self::NAME, Self::VERSION))
    }

    /// A structured health report: the host binary on `PATH`, then each configured
    /// agent's own checks. Reads state, never mutates.
    fn doctor() -> Result<DoctorReport> {
        crate::doctor::doctor(&Self::descriptor(), &Self::DEFAULT_SOURCE)
    }
}

/// Per-crate data root: `${XDG_DATA_HOME:-~/.local/share}/<plugin-name>/`, holding
/// `versions/`, the `current` pointer, and `markers/`.
pub(crate) fn data_root(plugin: &Plugin) -> Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| crate::error::Error::Tree("no data directory (XDG_DATA_HOME and HOME both unset)".into()))?;
    Ok(base.join(plugin.name))
}

#[cfg(test)]
#[path = "../tests/unit/host.rs"]
mod host_tests;
