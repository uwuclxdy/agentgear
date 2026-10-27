# How it works

The crate orchestrates the supported `claude plugin` CLI. The CLI is the transaction boundary and the source of truth for every install, update, uninstall and heal; the spawn-free `heal_needed_report(session)` pre-check reads Claude Code's registry files directly and answers `Needed` when a file its verdict depends on cannot be read or parsed, or on an `installed_plugins.json` schema version it does not recognize, so the decision falls back to the CLI.

## Lifecycle to CLI mapping

| op | sequence |
|---|---|
| `install(scope, source)` | acquire the shared lock. Ensure the marketplace (materialize + `marketplace add`, or `update` if present), then `plugin install <name>@<mkt>` if absent or stale, verified via `plugin list --json`. Write the stamp marker. |
| `update(scope)` | embedded: materialize a fresh versioned dir, `marketplace update`, `plugin update`. github: `plugin update`. Write the marker. |
| `uninstall(scope)` | per configured agent: skip without calling `remove` when the tool is not detected, has no surface at this scope, or (a config-merge backend) the resolved source is a GitHub ref it cannot render. Otherwise `remove(plugin, scope, source)` runs (claude: `plugin uninstall -y`, then a refcount-gated `marketplace remove`, only when no other installed plugin comes from that marketplace). The marker clears for every skipped or removed agent, so an explicit uninstall never orphans one; a failed `remove` keeps its marker so the next uninstall or self_heal retries it. |
| `self_heal(session)` | read the marker, `plugin list --json` (plus `marketplace list --json` when the entry reads healthy), then the state table below. |
| `heal_needed_report(session)` | the pre-check a host runs before `self_heal(session)`: reads files, spawns no CLI (backend detection is a PATH or config-directory lookup). One verdict per configured agent: `Needed`, `Clean` (`self_heal` would change nothing; a standing restart-pending flag counts as a change only under a `startup`/`resume` session), or `Skipped`. `claude` reads Claude Code's registry files beside agentgear's own marker, never the marker alone; every other backend the pre-check does not skip answers `Needed`. |
| `doctor()` | the six checks on the [Doctor](Doctor) page. |

Every `claude` call runs through one wrapper that locates the binary, scrubs the session env a hook would leak (`CLAUDECODE` and every `CLAUDE_CODE_*`, preserving `CLAUDE_CONFIG_DIR`), forces non-interactive stdio, and parses `--json` tolerantly.

Each mutating op above has a `_report` twin (`install_report`, `install_into_report`, `update_report`, `uninstall_report`, `self_heal_report`) returning an `AgentReport` instead of the merged `Outcome`: one `Converged`/`Skipped`/`Failed` status per configured agent, so a host can tell its user which agents converged, which were skipped and why, and which failed, rather than only the first real change. Full shape on [Types and errors](Types-and-Errors).

## Materialize (embedded and path sources)

The plugin tree ships as a compressed `.tar.br` blob baked into the binary (a pure-Rust brotli archive, roughly a quarter of the raw text size). Materialize decompresses it (`Source::Embedded`) or copies an on-disk tree (`Source::Path`) into a content-keyed versioned directory with an atomic pointer flip, scoped to the consuming client (`claude`, or `copilot-cli` for that backend; see [Agent backends](Agent-Backends)):

```text
~/.local/share/<name>/
  versions/<version>-<hash>@claude/   full tree + generated .claude-plugin/marketplace.json, ${AGENTGEAR_CLIENT} substituted for "claude"
  current@claude -> versions/<version>-<hash>@claude     symlink (unix) / junction (windows)
  markers/<hash>                      per-(plugin, scope, project) stamp
```

The tree is written to a temp sibling and fsynced, then renamed onto the target, which is created once so a rename never lands on a non-empty directory. `current@claude` is flipped by renaming a fresh pointer over it. A crash mid-materialize leaves the previous `current@claude` intact. The directory name carries a hash of the tree's own bytes, so an unchanged tree is reused as-is and a tree edited at an unchanged plugin version still lands in its own directory and gets the pointer; the version's superseded directories are then pruned, and other versions are kept. `marketplace add` points at `current@claude`, which Claude Code copies into its own cache keyed by version. A same-version tree change is therefore handed back to the CLI as an uninstall + install, the only sequence that replaces that copy. The client scoping keeps Claude Code and copilot-cli from colliding on one shared dir, since each needs its own `${AGENTGEAR_CLIENT}` substitution (claude copies the tree into its cache; copilot loads a local-marketplace plugin live).

## Self-heal state table

The hook ships inside the plugin, so `self_heal` only ever runs on an install that already exists. It repairs broken installs and never resurrects absent ones.

| stamp marker | plugin state | action |
|---|---|---|
| absent | absent | no-op |
| absent | present, healthy | adopt: write the marker |
| absent | present, broken or disabled | adopt: repair a break (a deliberate disable stays disabled), write the marker |
| present | absent (clean uninstall) | clear the marker, no-op (do not resurrect) |
| present | disabled | no-op (never re-enable) |
| present | broken, stale, or serving a tree the binary no longer ships | repair or update |
| present | healthy, current, and serving the binary's own tree | no-op |

The marker records the tree hash the harness was last handed, and Claude Code and copilot-cli read it back. With the marker gone there is nothing to check the harness's tree against, so at the running binary's own version those two repair where the table says adopt: the tree is handed over again and the hash recorded, and the session after that is a no-op. They still adopt where no hash is needed, meaning a GitHub source and an install newer than the running binary that another build of the same tool owns. Every other harness compares its own config directly and adopts as written.

Two invariants sit on top:

- **Never re-enable.** A disabled plugin is a deliberate choice, and self-heal never re-enables one. The one enable self-heal does is copilot-cli finishing a half-install: a row copilot lists `(disabled)` but never recorded as installed (no `enabledPlugins` key), left by a first install that failed after `marketplace add`. An explicit `install`/`update` does re-enable, because that is a direct user request.
- **Monotonic.** A registration at or above the embedded version is left alone when another build of the same tool owns it, so two coexisting binaries (a system package and a `cargo install` build) do not fight over the version each session, whether or not they share a data directory. What a newer version does not buy is protection for a registration nobody owns: one that is broken serves nothing, and a GitHub-sourced one under a binary that materializes its own tree serves the repo rather than any build. Both are repaired. Claude Code only: copilot-cli freezes on the version alone.

## Restart-pending flag

An out-of-band `setup update` re-materializes the plugin into Claude Code's cache and bumps the version, but the running session loaded the old plugin at session start. Claude Code does not hot-reload plugin hooks, so the session stays stale until the user runs `/reload-plugins` or restarts. A presence-only flag at `<data_root>/restart-pending` bridges that gap so the model can relay it.

`update()` and every `self_heal` branch that reconciles set the flag when that reconcile changed something, a same-version run whose plugin tree changed included, and a takeover of an install the binary never made along with it; `install()` never sets it. A reconcile that changed nothing clears the flag and the healthy branch does the same, both only under a `startup`- or `resume`-sourced SessionStart — the events that began by loading the plugin tree from disk, the only proof the running session is not stale; the marker-clear after a clean uninstall clears it under any source, the plugin being gone and the notice moot. The flag is advisory: a write or clear failure is swallowed, so it can never fail a lifecycle op that otherwise succeeded. A host `UserPromptSubmit` hook reads it through `PluginHost::restart_pending()` (the notice, or `None`) and prints the notice as plain stdout; a `startup`- or `resume`-sourced `SessionStart → self_heal` heal clears it. A hook gated on `heal_needed_report(session)` clears it too: the pre-check answers `Needed` whenever `self_heal` would clear a standing flag, so the session after a repair or an update runs one heal that retires it. Ship the `UserPromptSubmit` hook from your first release, since it fires from whatever version the running session has loaded.

## Concurrency

Every consumer of this crate shares one `flock` at a well-known path, held around each mutating sequence. Two different tools both self-healing at session start serialize instead of racing the registry.

## Versioning

Claude Code caches installed plugins keyed on the `plugin.json` version and never re-copies a tree under a version it already holds. agentgear covers that itself: a tree whose bytes changed is re-staged and handed back to the CLI as an uninstall + install, so a no-bump change still reaches installed users. The version stays a pin for everything else (the release tag, a GitHub `ref`, the never-downgrade comparison), lives only in `plugin.json`, equals `CARGO_PKG_VERSION`, and is enforced by the `build.rs` guard. The crate never writes a version into the generated marketplace entry.
