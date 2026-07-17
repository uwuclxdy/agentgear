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
use crate::materialize::{TreeSource, materialize};

pub(crate) struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn detect(&self) -> bool {
        which::which("claude").is_ok()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { plugins: true, mcp: true, hooks: true, scopes: &["user", "project"] }
    }

    /// The classification self_heal keys on: disabled beats broken beats stale.
    /// Mirrors selfheal.rs's inline logic (moved here so pass B can delegate to it).
    fn probe(&self, plugin: &Plugin, scope: &Scope) -> Result<BackendState> {
        let cli = ClaudeCli::locate()?;
        let Some(entry) = find_plugin(&cli, scope, plugin.name, plugin.marketplace)? else {
            return Ok(BackendState::Absent);
        };
        if entry.enabled == Some(false) {
            return Ok(BackendState::Disabled);
        }
        let files_ok = entry.install_path.as_ref().is_none_or(|p| Path::new(p).exists());
        let monotonic_current = !version_lt(entry.version.as_deref(), plugin.version);
        Ok(if files_ok && monotonic_current { BackendState::Healthy } else { BackendState::NeedsRepair })
    }

    fn reconcile(&self, plugin: &Plugin, desired: &Desired, scope: &Scope) -> Result<Outcome> {
        reconcile(plugin, desired, scope)
    }

    fn remove(&self, plugin: &Plugin, scope: &Scope) -> Result<Outcome> {
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
    Ok(entries.into_iter().find(|e| e.matches(name, marketplace)))
}

fn find_marketplace(cli: &ClaudeCli, scope: &Scope, marketplace: &str) -> Result<Option<MarketplaceEntry>> {
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
    let cli = ClaudeCli::locate()?;
    let marketplace = find_marketplace(&cli, scope, plugin.marketplace)?;
    let entry = find_plugin(&cli, scope, plugin.name, plugin.marketplace)?;
    let id = plugin.id();

    let Some(entry) = entry else {
        // Absent: full install.
        cli.ensure_min_version()?;
        ensure_marketplace(&cli, plugin, &desired.source, scope, marketplace.as_ref())?;
        plugin_install(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        return Ok(Outcome::Installed);
    };

    if entry.enabled == Some(false) {
        // An explicit install/update honors the user's intent and re-enables (the
        // design says install flips enable state). self_heal/adopt never does.
        if !desired.reenable {
            return Ok(Outcome::NoOp);
        }
        cli.ensure_min_version()?;
        plugin_enable(&cli, &id, scope)?;
        return Ok(Outcome::Repaired);
    }

    let installed = entry.version.clone();
    let stale = version_lt(installed.as_deref(), plugin.version);
    let newer = installed.as_deref().is_some_and(|v| version_lt(Some(plugin.version), v));
    let structural_ok = structural_ok(&entry, marketplace.as_ref(), &desired.source);

    // Monotonic: a strictly-newer install belongs to a newer binary. Never touch
    // it — not even to repair a broken one — or two coexisting binaries downgrade
    // each other on every session. The newer binary owns its own repair.
    if newer {
        return Ok(Outcome::NoOp);
    }

    if structural_ok && !stale {
        return Ok(Outcome::NoOp); // healthy and monotonic-satisfied (installed == embedded)
    }

    cli.ensure_min_version()?;
    ensure_marketplace(&cli, plugin, &desired.source, scope, marketplace.as_ref())?;

    if stale && structural_ok {
        plugin_update(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        Ok(Outcome::Updated { from: installed, to: plugin.version.to_string() })
    } else {
        // Structurally broken (registered but files/marketplace gone): clean reinstall.
        let _ = plugin_uninstall(&cli, &id, scope);
        plugin_install(&cli, &id, scope)?;
        verify_present(&cli, scope, plugin)?;
        Ok(Outcome::Repaired)
    }
}

/// Embedded/path: (re)materialize so `current` is fresh, then add-if-absent /
/// update-if-present. GitHub: send `owner/repo@ref` to pin the ref; when already
/// present, `update` if the stored ref still matches, else re-`add` to re-point the
/// pin (`update` never moves one — design §ref-pinning ground truth).
fn ensure_marketplace(cli: &ClaudeCli, plugin: &Plugin, source: &Source, scope: &Scope, present: Option<&MarketplaceEntry>) -> Result<()> {
    let source_str = match source {
        Source::Embedded => materialize(plugin, TreeSource::Blob(plugin.blob()))?.display().to_string(),
        // A path source materializes its on-disk tree the same way embedded does.
        Source::Path(p) => materialize(plugin, TreeSource::Dir(p))?.display().to_string(),
        Source::GitHub { repo, ref_ } => github_source(repo, ref_),
    };
    match marketplace_op(source, present) {
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
/// or re-point a github pin whose stored ref drifted from the desired one (`update`
/// never moves a pin). `Update` refreshes an already-present marketplace sitting on
/// its desired ref, and every non-github source.
#[derive(Debug, PartialEq, Eq)]
enum MarketplaceOp {
    Add,
    Update,
}

fn marketplace_op(source: &Source, present: Option<&MarketplaceEntry>) -> MarketplaceOp {
    match (source, present) {
        (_, None) => MarketplaceOp::Add,
        (Source::GitHub { ref_, .. }, Some(entry)) if entry.ref_.as_deref() != Some(*ref_) => MarketplaceOp::Add,
        (_, Some(_)) => MarketplaceOp::Update,
    }
}

fn structural_ok(entry: &PluginEntry, marketplace: Option<&MarketplaceEntry>, source: &Source) -> bool {
    let files_ok = entry.install_path.as_ref().is_none_or(|p| Path::new(p).exists());
    let marketplace_ok = match source {
        // Embedded/path: the local marketplace path must still resolve (not moved/deleted).
        Source::Embedded | Source::Path(_) => marketplace.is_some_and(|m| m.path.as_ref().is_none_or(|p| Path::new(p).exists())),
        Source::GitHub { .. } => marketplace.is_some(),
    };
    files_ok && marketplace_ok
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
