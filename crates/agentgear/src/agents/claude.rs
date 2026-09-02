//! The Claude Code backend: converge the `claude plugin` registry to a desired
//! state via marketplace-ensure + install/update, read back through `list --json`.
//! It orchestrates the supported CLI; it never forges CC's on-disk state.

use std::path::Path;

use super::{AgentBackend, BackendState};
use crate::cli::{ClaudeCli, version_lt};
use crate::doctor::DoctorReport;
use crate::error::{Error, Result};
use crate::host::{Capabilities, Desired, Outcome, Plugin, Scope, Source};
use crate::manifest::{MarketplaceEntry, PluginEntry};
use crate::materialize::{TreeSource, content_hash, materialize};
use crate::stamp;

pub(crate) struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn detect(&self) -> bool {
        which::which("claude").is_ok()
    }

    fn capabilities(&self) -> Capabilities {
        // Plugin-native: the `claude plugin` install copies the whole CC tree, so
        // every surface is served natively. `instructions` stays false: it is the
        // non-CC context-file surface, and CC receives host guidance via the MCP
        // `instructions` channel instead.
        Capabilities {
            plugins: true,
            mcp: true,
            hooks: true,
            commands: true,
            agents: true,
            skills: true,
            instructions: false,
            scopes: &["user", "project"],
        }
    }

    /// The classification self_heal keys on: disabled beats broken beats stale.
    fn probe(&self, plugin: &Plugin, scope: &Scope, source: &Source) -> Result<BackendState> {
        // CLI-based: the `claude plugin` registry is the source of truth; the
        // resolved `source` enters only the marketplace-health classifier below.
        let cli = ClaudeCli::locate()?;
        let Some(entry) = find_plugin(&cli, scope, plugin.name, plugin.marketplace)? else {
            return Ok(BackendState::Absent);
        };
        if entry.enabled == Some(false) {
            return Ok(BackendState::Disabled);
        }
        // A non-empty `errors` list means CC computed a load failure for this
        // entry (a marketplace whose manifest vanished registers 0 hooks and 0
        // MCP while its files still resolve), so it outranks the file/version
        // verdicts — a heal that read such an entry as Healthy would stamp a
        // marker and never repair it (probed 2.1.241).
        let errors_ok = entry.errors.as_ref().is_none_or(Vec::is_empty);
        let files_ok = entry.install_path.as_ref().is_none_or(|p| Path::new(p).exists());
        let monotonic_current = !version_lt(entry.version.as_deref(), plugin.version);
        let newer = entry.version.as_deref().is_some_and(|v| version_lt(Some(plugin.version), v));
        let registry = if errors_ok && files_ok && monotonic_current { BackendState::Healthy } else { BackendState::NeedsRepair };
        // A healthy-looking entry can still sit on a divergent or broken
        // marketplace — a registration elsewhere than the materialized pointer
        // loads fine and reports no errors until its source loses the manifest —
        // so the marketplace health folds in before the verdict. That is the
        // second read a heal run pays for; the divergence signal exists nowhere
        // on the plugin entry.
        let registry = if matches!(registry, BackendState::Healthy) {
            let marketplace = find_marketplace(&cli, scope, plugin.marketplace)?;
            let expected = crate::host::data_root(plugin)?.join(format!("current@{}", ClaudeBackend.id()));
            let healthy = matches!(marketplace_health(marketplace.as_ref(), source, &expected), MarketplaceHealth::Healthy)
                // The tree CC holds is the third thing a healthy-looking entry can be
                // wrong about, and the only one no read of CC's own state can see: its
                // cache copy is version-keyed, so an edited tree at an unchanged version
                // leaves the entry, its files and its version all correct. Without this
                // term self_heal's `(marker present, Healthy)` row no-ops forever and a
                // box converges only on an explicit `setup`.
                //
                // Monotonic outranks it, matching `reconcile`'s `Frozen`: a strictly-newer
                // install holds a newer binary's tree, which never matches our hash, so
                // folding it in would spawn a reconcile every session for that no-op to
                // throw away.
                && (newer || tree_is_current(plugin, scope, staged_tree_hash(plugin, source).as_deref())?);
            if healthy { BackendState::Healthy } else { BackendState::NeedsRepair }
        } else {
            registry
        };
        // The registry alone decides presence.
        Ok(registry)
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        reconcile(plugin, desired, scope)
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope, _source: &Source) -> Result<Outcome> {
        remove(plugin, scope)
    }

    fn report(&self, plugin: &Plugin, source: &Source) -> DoctorReport {
        // The claude-specific checks only; the doctor fan-out owns the shared
        // host-binary check (calling `doctor` here would recurse through `report`).
        crate::doctor::claude_report(plugin, source)
    }
}

// --- state reads -------------------------------------------------------------

pub(crate) fn find_plugin(cli: &ClaudeCli, scope: &Scope, name: &str, marketplace: &str) -> Result<Option<PluginEntry>> {
    let entries: Vec<PluginEntry> = cli.run_json(&["plugin", "list", "--json"], scope.cwd(), "plugin list --json")?;
    // Scope-filtered: the same id installed at both user and project scope must
    // not let a user-scope op read the project entry first (design §self_heal's
    // former scope-blind caveat; the dead-entry shape a per-session config dir
    // leaves behind makes the wrong first match a standing heal failure).
    Ok(entries.into_iter().find(|e| e.matches(name, marketplace) && e.at_scope(scope)))
}

pub(crate) fn find_marketplace(cli: &ClaudeCli, scope: &Scope, marketplace: &str) -> Result<Option<MarketplaceEntry>> {
    let entries: Vec<MarketplaceEntry> =
        cli.run_json(&["plugin", "marketplace", "list", "--json"], scope.cwd(), "marketplace list --json")?;
    Ok(entries.into_iter().find(|m| m.name.as_deref() == Some(marketplace)))
}

fn marketplace_has_installed(cli: &ClaudeCli, scope: &Scope, marketplace: &str) -> Result<bool> {
    let entries: Vec<PluginEntry> = cli.run_json(&["plugin", "list", "--json"], scope.cwd(), "plugin list --json")?;
    Ok(entries.iter().any(|e| e.marketplace() == Some(marketplace)))
}

// --- mutating calls ----------------------------------------------------------

fn marketplace_add(cli: &ClaudeCli, source: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "marketplace", "add", source, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn marketplace_update(cli: &ClaudeCli, marketplace: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "marketplace", "update", marketplace], scope.cwd())?;
    Ok(())
}

fn marketplace_remove(cli: &ClaudeCli, marketplace: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "marketplace", "remove", marketplace, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn plugin_install(cli: &ClaudeCli, id: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "install", id, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn plugin_update(cli: &ClaudeCli, id: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "update", id, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn plugin_uninstall(cli: &ClaudeCli, id: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "uninstall", id, "-y", "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

fn plugin_enable(cli: &ClaudeCli, id: &str, scope: &Scope) -> Result<()> {
    cli.run(&["plugin", "enable", id, "--scope", scope.as_cli()], scope.cwd())?;
    Ok(())
}

// --- reconcile ---------------------------------------------------------------

pub(crate) fn reconcile(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
    reconcile_registry(plugin, desired, scope)
}

fn reconcile_registry(plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
    let cli = ClaudeCli::locate()?;
    let marketplace = find_marketplace(&cli, scope, plugin.marketplace)?;
    let entry = find_plugin(&cli, scope, plugin.name, plugin.marketplace)?;
    let id = plugin.id();
    // The tree this pass would stage, hashed before anything is written: CC copies the
    // tree into its own cache at install time and keys that cache on the plugin VERSION,
    // so an edited tree at an unchanged version reaches CC only through a reinstall.
    let staged = staged_tree_hash(plugin, &desired.source);

    let Some(entry) = entry else {
        // Absent: full install.
        cli.ensure_min_version()?;
        ensure_marketplace(&cli, plugin, &desired.source, scope, marketplace.as_ref())?;
        plugin_install(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        record_staged(plugin, desired, scope, staged.as_deref())?;
        return Ok(Outcome::Installed);
    };

    // Re-enabling does not return: a disabled entry can also be stale or holding an
    // edited tree, and reporting `Repaired` off the enable alone would call a pass
    // converged that left the box on the bytes it already had.
    let mut re_enabled = false;
    if entry.enabled == Some(false) {
        // An explicit install/update honors the user's intent and re-enables (the
        // design says install flips enable state). self_heal/adopt never does.
        if !desired.reenable {
            return Ok(Outcome::NoOp);
        }
        cli.ensure_min_version()?;
        plugin_enable(&cli, &id, scope)?;
        re_enabled = true;
    }

    let installed = entry.version.clone();
    let stale = version_lt(installed.as_deref(), plugin.version);
    let newer = installed.as_deref().is_some_and(|v| version_lt(Some(plugin.version), v));
    // The path materialize will (re)publish; a registered source diverging from it
    // (an old checkout dir, a github entry under an embedded host, a pre-client-
    // scoping pointer) is structural damage a heal repairs by re-pointing.
    let expected = crate::host::data_root(plugin)?.join(format!("current@{}", ClaudeBackend.id()));
    // Monotonic's own structural test, and the reason it takes a different `expected`:
    // it protects a strictly-newer install that ANOTHER BINARY OF THIS TOOL owns, so
    // the registered path is read against ITSELF rather than against our pointer. An
    // entry sitting at another binary's materialized dir is that binary's working
    // install; comparing it to ours instead is what makes two binaries on different
    // data roots re-point each other every session, forever. Every other term still
    // bites, and none of those shapes is anyone's working install: a github entry
    // under a local source tracks the repo rather than a binary, while an absent
    // marketplace, a vanished manifest, missing files and a CC-computed load error
    // each serve nothing at all.
    let registered = marketplace.as_ref().and_then(|m| m.path.as_deref()).map(Path::new).unwrap_or(expected.as_path());
    let owned_by_another_binary = structural_ok(&entry, marketplace.as_ref(), &desired.source, registered);
    let structural_ok = structural_ok(&entry, marketplace.as_ref(), &desired.source, &expected);

    // Never downgrade or re-hand a tree to a newer binary's install. `probe` cannot
    // express "another binary owns it" — it knows only our pointer — so on such a box
    // it reports `NeedsRepair` and this answers with a no-op: two registry reads per
    // session, no mutation, which is the price of not thrashing.
    if newer && owned_by_another_binary {
        return Ok(Outcome::NoOp);
    }

    if structural_ok && !stale && tree_is_current(plugin, scope, staged.as_deref())? {
        // Healthy, monotonic-satisfied (installed == embedded), and serving the tree
        // this binary ships. A re-enable above is still a change, so it reports one.
        let outcome = if re_enabled { Outcome::Repaired } else { Outcome::NoOp };
        return Ok(outcome);
    }

    cli.ensure_min_version()?;
    ensure_marketplace(&cli, plugin, &desired.source, scope, marketplace.as_ref())?;

    if stale && structural_ok {
        plugin_update(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        record_staged(plugin, desired, scope, staged.as_deref())?;
        Ok(Outcome::Updated { from: installed, to: plugin.version.to_string() })
    } else {
        // Structurally broken (registered but files/marketplace gone), or serving a
        // tree this binary no longer ships at a version that will never bump: clean
        // reinstall, the one sequence that re-copies a same-version tree into CC's
        // own cache (design § marketplace ground truth).
        // Marked before the uninstall, never after: a failed `plugin install` or a
        // SessionStart hook killed between the two calls otherwise leaves a marker
        // beside an absent plugin, which self_heal reads as the user's own uninstall
        // and forgets for good.
        stamp::begin_reinstall(plugin, scope, &desired.source, ClaudeBackend.id())?;
        let _ = plugin_uninstall(&cli, &id, scope);
        plugin_install(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        record_staged(plugin, desired, scope, staged.as_deref())?;
        Ok(Outcome::Repaired)
    }
}

/// The hash of the tree this reconcile would stage for CC.
///
/// `None` covers both "there is no local tree" (github, which CC tracks by ref) and
/// "the local tree cannot be read right now" — a `--path` checkout that moved, a
/// reaped worktree, a zero-embed binary. Both mean no drift is DETECTABLE, never that
/// the tree drifted: a healthy install whose source went away used to converge to a
/// silent no-op, and turning that into a hard failure would red every session-start
/// heal from then on. A source that must be read to converge still fails loudly the
/// moment `ensure_marketplace` materializes from it.
fn staged_tree_hash(plugin: &Plugin, source: &Source) -> Option<String> {
    let client = ClaudeBackend.id();
    match source {
        Source::Embedded => content_hash(TreeSource::Blob(plugin.blob()), client).ok(),
        Source::Path(p) => content_hash(TreeSource::Dir(p), client).ok(),
        Source::GitHub { .. } => None,
    }
}

/// Whether CC already holds the tree `staged` names, read off this agent's own stamp
/// marker. Unknown content converges rather than assuming freshness: a github source
/// has no tree to compare (always current), while a marker that is absent or predates
/// the record leaves what CC copied unaccounted for, and the version comparison can
/// never account for it either.
fn tree_is_current(plugin: &Plugin, scope: &Scope, staged: Option<&str>) -> Result<bool> {
    let Some(staged) = staged else {
        return Ok(true);
    };
    let marker = stamp::read(plugin, scope, ClaudeBackend.id())?;
    Ok(marker.and_then(|m| m.tree_hash).is_some_and(|recorded| recorded == staged))
}

/// Record the tree CC now holds, after the call that handed it over succeeded, and end
/// any reinstall this pass began. A github source records no hash — it has no local
/// tree, and overwriting an earlier local record would hide the drift of a host
/// switching back — but it still ends the reinstall.
fn record_staged(plugin: &Plugin, desired: &Desired, scope: &Scope, staged: Option<&str>) -> Result<()> {
    stamp::record_converged(plugin, scope, &desired.source, ClaudeBackend.id(), staged)
}

/// Embedded/path: (re)materialize so `current@claude` is fresh, then add-if-absent /
/// update-if-present. GitHub: send `owner/repo@ref` to pin the ref; when already
/// present, `update` if the stored ref still matches, else re-`add` to re-point the
/// pin (`update` never moves one — design §ref-pinning ground truth). A present
/// local-source entry whose registered path diverges from the just-materialized
/// pointer (an old checkout dir, a github-registered entry, a pre-client-scoping
/// `current`) is re-pointed the same way: `marketplace add <dir>` over the same
/// name re-registers the entry in place, while `update` only re-fetches the stale
/// source and fails on it (probed 2.1.241).
fn ensure_marketplace(cli: &ClaudeCli, plugin: &Plugin, source: &Source, scope: &Scope, present: Option<&MarketplaceEntry>) -> Result<()> {
    // Client-scope the materialization under this backend's own id, so CC and copilot
    // never collide on the shared data root (each bakes its own `${AGENTGEAR_CLIENT}`).
    let client = ClaudeBackend.id();
    let source_str = match source {
        Source::Embedded => materialize(plugin, TreeSource::Blob(plugin.blob()), client)?.display().to_string(),
        // A path source materializes its on-disk tree the same way embedded does.
        Source::Path(p) => materialize(plugin, TreeSource::Dir(p), client)?.display().to_string(),
        Source::GitHub { repo, ref_ } => github_source(repo, ref_),
    };
    match marketplace_op(source, present, &source_str) {
        MarketplaceOp::Update => marketplace_update(cli, plugin.marketplace, scope)?,
        MarketplaceOp::Add => marketplace_add(cli, &source_str, scope)?,
    }
    Ok(())
}

/// The `marketplace add` source string a github pin sends: `owner/repo@ref`, which
/// the CLI resolves by checking `ref` out (design §ref-pinning ground truth). A bare
/// `owner/repo` silently tracks the default branch instead.
fn github_source(repo: &str, ref_: &str) -> String {
    format!("{repo}@{ref_}")
}

/// Present-branch decision. `Add` re-registers the marketplace: install-if-absent,
/// re-point a github pin whose stored ref drifted from the desired one (`update`
/// never moves a pin), or re-point a local-source entry whose registered path
/// diverged from the just-materialized pointer — `expected` — including a
/// github-registered entry met by an embedded/path host (the migration case) and a
/// pre-client-scoping `current` pointer. `Update` refreshes an already-present
/// marketplace sitting on its desired source.
#[derive(Debug, PartialEq, Eq)]
enum MarketplaceOp {
    Add,
    Update,
}

fn marketplace_op(source: &Source, present: Option<&MarketplaceEntry>, expected: &str) -> MarketplaceOp {
    match (source, present) {
        (_, None) => MarketplaceOp::Add,
        (Source::GitHub { ref_, .. }, Some(entry)) if entry.ref_.as_deref() != Some(*ref_) => MarketplaceOp::Add,
        (Source::Embedded | Source::Path(_), Some(entry)) if local_entry_diverges(entry, expected) => MarketplaceOp::Add,
        (_, Some(_)) => MarketplaceOp::Update,
    }
}

/// A present marketplace entry that a local-source reconcile must re-point rather
/// than update: its own source kind is github, or its registered path is not the
/// materialized pointer the reconcile just staged.
fn local_entry_diverges(entry: &MarketplaceEntry, expected: &str) -> bool {
    entry.source.as_deref() == Some("github") || entry.path.as_deref() != Some(expected)
}

/// A local marketplace's health. `Dangling` covers every shape a heal must repair
/// by re-pointing: a moved/deleted path, a path whose generated manifest vanished,
/// a github-registered entry under a local desired source, and a registered path
/// that diverged from the materialized pointer `expected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarketplaceHealth {
    Healthy,
    Absent,
    Dangling,
}

pub(crate) fn marketplace_health(marketplace: Option<&MarketplaceEntry>, source: &Source, expected: &Path) -> MarketplaceHealth {
    let Some(m) = marketplace else {
        return MarketplaceHealth::Absent;
    };
    match source {
        // A github entry has no local path to dangle (the registry stores source +
        // ref), so it reads healthy only under a github desired source — the same
        // entry under an embedded/path host is divergence to re-point.
        Source::GitHub { .. } => MarketplaceHealth::Healthy,
        Source::Embedded | Source::Path(_) => {
            if m.source.as_deref() == Some("github") {
                return MarketplaceHealth::Dangling;
            }
            let Some(path) = m.path.as_deref() else {
                return MarketplaceHealth::Dangling;
            };
            if Path::new(path) != expected {
                return MarketplaceHealth::Dangling;
            }
            if !Path::new(path).join(".claude-plugin").join("marketplace.json").exists() {
                return MarketplaceHealth::Dangling;
            }
            MarketplaceHealth::Healthy
        }
    }
}

fn structural_ok(entry: &PluginEntry, marketplace: Option<&MarketplaceEntry>, source: &Source, expected: &Path) -> bool {
    let files_ok = entry.install_path.as_ref().is_none_or(|p| Path::new(p).exists());
    let marketplace_ok = matches!(marketplace_health(marketplace, source, expected), MarketplaceHealth::Healthy);
    let errors_ok = entry.errors.as_ref().is_none_or(Vec::is_empty);
    files_ok && marketplace_ok && errors_ok
}

fn verify_present(cli: &ClaudeCli, scope: &Scope, plugin: &Plugin) -> Result<()> {
    match find_plugin(cli, scope, plugin.name, plugin.marketplace)? {
        Some(e) if e.install_path.as_ref().is_none_or(|p| Path::new(p).exists()) => Ok(()),
        Some(_) => Err(Error::Verify(format!("{} registered but its files are missing after the operation", plugin.id()))),
        None => Err(Error::Verify(format!("{} absent from `plugin list --json` after the operation", plugin.id()))),
    }
}

// --- remove ------------------------------------------------------------------

pub(crate) fn remove(plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
    let cli = ClaudeCli::locate()?;
    let id = plugin.id();

    if find_plugin(&cli, scope, plugin.name, plugin.marketplace)?.is_some() {
        plugin_uninstall(&cli, &id, scope)?;
    }

    // Refcount-gated: only drop the marketplace when no plugin from it remains.
    if !marketplace_has_installed(&cli, scope, plugin.marketplace)? && find_marketplace(&cli, scope, plugin.marketplace)?.is_some() {
        let _ = marketplace_remove(&cli, plugin.marketplace, scope);
    }
    Ok(Outcome::Removed)
}

#[cfg(test)]
#[path = "../../tests/unit/claude.rs"]
mod claude_tests;
