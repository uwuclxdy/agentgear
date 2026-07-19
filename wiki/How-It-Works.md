# How it works

The crate orchestrates the supported `claude plugin` CLI. The CLI is the transaction boundary and the source of truth, so a Claude Code registry schema bump never breaks the crate.

## Lifecycle to CLI mapping

| op | sequence |
|---|---|
| `install(scope, source)` | acquire the shared lock. Ensure the marketplace (materialize + `marketplace add`, or `update` if present), then `plugin install <name>@<mkt>` if absent or stale, verified via `plugin list --json`. Write the stamp marker. |
| `update(scope)` | embedded: materialize a fresh versioned dir, `marketplace update`, `plugin update`. github: `plugin update`. Write the marker. |
| `uninstall(scope)` | per configured agent: skip without calling `remove` when the tool is not detected, has no surface at this scope, or (a config-merge backend) the resolved source is a GitHub ref it cannot render. Otherwise `remove(plugin, scope, source)` runs (claude: `plugin uninstall -y`, then a refcount-gated `marketplace remove`, only when no other installed plugin comes from that marketplace). The marker clears for every skipped or removed agent, so an explicit uninstall never orphans one; a failed `remove` keeps its marker so the next uninstall or self_heal retries it. |
| `self_heal()` | read the marker, one `plugin list --json`, then the state table below. |
| `doctor()` | the six checks on the [Doctor](Doctor) page. |

Every call runs through one wrapper that locates `claude`, scrubs the session env a hook would leak (`CLAUDECODE` and every `CLAUDE_CODE_*`, preserving `CLAUDE_CONFIG_DIR`), forces non-interactive stdio, and parses `--json` tolerantly.

Each op above has a `_report` twin (`install_report`, `install_into_report`, `update_report`, `uninstall_report`, `self_heal_report`) returning an `AgentReport` instead of the merged `Outcome`: one `Converged`/`Skipped`/`Failed` status per configured agent, so a host can tell its user which agents converged, which were skipped and why, and which failed, rather than only the first real change. Full shape on [Types and errors](Types-and-Errors).

## Materialize (embedded and path sources)

The plugin tree ships as a compressed `.tar.br` blob baked into the binary (a pure-Rust brotli archive, roughly a quarter of the raw text size). Materialize decompresses it (`Source::Embedded`) or copies an on-disk tree (`Source::Path`) into a content-keyed versioned directory with an atomic pointer flip:

```text
~/.local/share/<name>/
  versions/<version>/               full tree + generated .claude-plugin/marketplace.json
  current -> versions/<version>     symlink (unix) / junction (windows)
  markers/<hash>                    per-(plugin, scope, project) stamp
```

The tree is written to a temp sibling and fsynced, then renamed onto the versioned target, which is created once so a rename never lands on a non-empty directory. `current` is flipped by renaming a fresh pointer over it. A crash mid-materialize leaves the previous `current` intact. An existing version dir is reused without re-decompressing. `marketplace add` points at `current`, which Claude Code copies into its own cache keyed by version.

## Self-heal state table

The hook ships inside the plugin, so `self_heal` only ever runs on an install that already exists. It repairs broken installs and never resurrects absent ones.

| stamp marker | plugin state | action |
|---|---|---|
| absent | absent | no-op |
| absent | present, healthy | adopt: write the marker |
| absent | present, broken or disabled | adopt: repair a break (a disable stays disabled), write the marker |
| present | absent (clean uninstall) | clear the marker, no-op (do not resurrect) |
| present | disabled | no-op (never re-enable) |
| present | broken or stale | repair or update |
| present | healthy and current | no-op |

Two invariants sit on top:

- **Never re-enable.** A disabled plugin is a deliberate choice; self-heal repairs structure, not enable state. An explicit `install`/`update` does re-enable, because that is a direct user request.
- **Monotonic.** When the installed version is at or above the embedded version, self-heal does nothing, so two coexisting binaries of the same tool (a system package and a `cargo install` build) do not fight over the version each session.

## Restart-pending flag

An out-of-band `setup update` re-materializes the plugin into Claude Code's cache and bumps the version, but the running session loaded the old plugin at session start. Claude Code does not hot-reload plugin hooks, so the session stays stale until the user runs `/reload-plugins` or restarts. A presence-only flag at `<data_root>/restart-pending` bridges that gap so the model can relay it.

`update()` and `self_heal()`'s repair branch set the flag when the reconcile changed something (a same-version re-run sets nothing); `install()` never sets it. Every other `self_heal` branch that touches Claude Code clears it: healthy, adopt, and the marker-clear after a clean uninstall all mean the running session is not stale. The flag is advisory: a write or clear failure is swallowed, so it can never fail a lifecycle op that otherwise succeeded. A host `UserPromptSubmit` hook reads it through `PluginHost::restart_pending()` (the notice, or `None`) and prints the notice as plain stdout; the `SessionStart → self_heal` hook clears it. Ship the `UserPromptSubmit` hook from your first release, since it fires from whatever version the running session has loaded.

## Concurrency

Every consumer of this crate shares one `flock` at a well-known path, held around each mutating sequence. Two different tools both self-healing at session start serialize instead of racing the registry.

## Versioning

Claude Code caches installed plugins keyed on the `plugin.json` version, so pushing a change without a bump is a silent no-op for installed users. The version lives only in `plugin.json`, equal to `CARGO_PKG_VERSION`, enforced by the `build.rs` guard. The crate never writes a version into the generated marketplace entry.
