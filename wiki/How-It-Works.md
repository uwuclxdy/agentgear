# How it works

The crate orchestrates the supported `claude plugin` CLI. The CLI is the transaction boundary and the source of truth, so a Claude Code registry schema bump never breaks the crate.

## Lifecycle to CLI mapping

| op | sequence |
|---|---|
| `install(scope, source)` | acquire the shared lock. Ensure the marketplace (materialize + `marketplace add`, or `update` if present), then `plugin install <name>@<mkt>` if absent or stale, verified via `plugin list --json`. Write the stamp marker. |
| `update(scope)` | embedded: materialize a fresh versioned dir, `marketplace update`, `plugin update`. github: `plugin update`. Write the marker. |
| `uninstall(scope)` | `plugin uninstall -y`, then a refcount-gated `marketplace remove` (only when no other installed plugin comes from that marketplace). Clear the marker. |
| `self_heal()` | read the marker, one `plugin list --json`, then the state table below. |
| `doctor()` | the six checks on the [Doctor](Doctor) page. |

Every call runs through one wrapper that locates `claude`, scrubs the session env a hook would leak (`CLAUDECODE` and every `CLAUDE_CODE_*`, preserving `CLAUDE_CONFIG_DIR`), forces non-interactive stdio, and parses `--json` tolerantly.

## Materialize (embedded mode)

The embedded tree becomes a content-keyed versioned directory with an atomic pointer flip:

```text
~/.local/share/<name>/
  versions/<version>/               full tree + generated .claude-plugin/marketplace.json
  current -> versions/<version>     symlink (unix) / junction (windows)
  markers/<hash>                    per-(plugin, scope, project) stamp
```

The tree is written to a temp sibling and fsynced, then renamed onto the versioned target, which is created once so a rename never lands on a non-empty directory. `current` is flipped by renaming a fresh pointer over it. A crash mid-materialize leaves the previous `current` intact. `marketplace add` points at `current`, which Claude Code copies into its own cache keyed by version.

## Self-heal state table

The hook ships inside the plugin, so `self_heal` only ever runs on an install that already exists. It repairs broken installs and never resurrects absent ones.

| stamp marker | plugin state | action |
|---|---|---|
| absent | absent | no-op |
| absent | present, healthy | adopt: write the marker |
| absent | present, broken | repair, write the marker |
| present | absent (clean uninstall) | clear the marker, no-op (do not resurrect) |
| present | broken or stale | repair or update |
| present | healthy and current | no-op |

Two invariants sit on top:

- **Never re-enable.** A disabled plugin is a deliberate choice; self-heal repairs structure, not enable state. An explicit `install`/`update` does re-enable, because that is a direct user request.
- **Monotonic.** When the installed version is at or above the embedded version, self-heal does nothing, so two coexisting binaries of the same tool (a system package and a `cargo install` build) do not fight over the version each session.

## Concurrency

Every consumer of this crate shares one `flock` at a well-known path, held around each mutating sequence. Two different tools both self-healing at session start serialize instead of racing the registry.

## Versioning

Claude Code caches installed plugins keyed on the `plugin.json` version, so pushing a change without a bump is a silent no-op for installed users. The version lives only in `plugin.json`, equal to `CARGO_PKG_VERSION`, enforced by the `build.rs` guard. The crate never writes a version into the generated marketplace entry.
