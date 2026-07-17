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
    User,
    Project { path: PathBuf },
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

    /// Stable key fragment for the stamp marker hash.
    pub(crate) fn key(&self) -> String {
        match self {
            Scope::User => "user".to_string(),
            Scope::Project { path } => format!("project:{}", path.display()),
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
    GitHub {
        repo: &'static str,
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
    Installed,
    Updated {
        from: Option<String>,
        to: String,
    },
    /// A broken/partial state was reconciled back to healthy.
    Repaired,
    /// self_heal found a healthy install with no marker and wrote one.
    Adopted,
    Removed,
    /// self_heal found a cleanly-uninstalled plugin and cleared the stale marker.
    Cleared,
}

/// What an agent backend can host. Lets `setup` report "codex: mcp only" instead
/// of silently dropping features.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub plugins: bool,
    pub mcp: bool,
    pub hooks: bool,
    pub scopes: &'static [&'static str],
}

/// A resolved plugin descriptor. Built by [`PluginHost::descriptor`] from the
/// derive-emitted metadata; passed to backends.
#[derive(Clone)]
pub struct Plugin {
    pub name: &'static str,
    pub marketplace: &'static str,
    pub version: &'static str,
    pub agents: &'static [&'static str],
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
    const NAME: &'static str;
    const MARKETPLACE: &'static str;
    const VERSION: &'static str;
    const DEFAULT_SOURCE: Source;
    const AGENTS: &'static [&'static str];

    /// The plugin tree baked into the host crate as a compressed `.tar.br` blob
    /// (the derive's `include_bytes!`). Empty when the derive's `embed` attr is
    /// off; [`Source::Embedded`] then errors at materialize.
    fn embedded_blob() -> &'static [u8];

    fn descriptor() -> Plugin {
        Plugin {
            name: Self::NAME,
            marketplace: Self::MARKETPLACE,
            version: Self::VERSION,
            agents: Self::AGENTS,
            blob: Self::embedded_blob(),
        }
    }

    /// Idempotent: ensure the plugin is installed at the embedded version.
    fn install(scope: Scope, source: Source) -> Result<Outcome> {
        crate::install::install(&Self::descriptor(), scope, source)
    }

    /// Like [`PluginHost::install`] but only into the `AGENTS` whose id is in
    /// `agents` (an empty slice = all of `AGENTS`). Lets a host target one backend
    /// (`setup --agent gemini`) without touching the others.
    fn install_into(scope: Scope, source: Source, agents: &[&str]) -> Result<Outcome> {
        crate::install::install_filtered(&Self::descriptor(), scope, source, agents)
    }

    /// Materialize a new versioned tree, then update the marketplace + plugin.
    fn update(scope: Scope) -> Result<Outcome> {
        crate::install::update(&Self::descriptor(), scope, Self::DEFAULT_SOURCE)
    }

    /// Uninstall, then refcount-gated marketplace remove; clears the marker.
    fn uninstall(scope: Scope) -> Result<Outcome> {
        crate::install::uninstall(&Self::descriptor(), scope)
    }

    /// SessionStart entrypoint. Repairs broken installs, never resurrects a
    /// deliberate uninstall, never downgrades, never re-enables (design §6).
    fn self_heal() -> Result<Outcome> {
        crate::selfheal::self_heal(&Self::descriptor(), Self::DEFAULT_SOURCE)
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
