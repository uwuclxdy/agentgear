# Changelog

## 0.1.0-rc.1 - 2026-07-19

First public release candidate.

### Features
- `#[derive(PluginHost)]` gave a binary the whole plugin lifecycle: `install`, `update`, `uninstall`, `self_heal`, `doctor`.
- 25 agent backends, each behind its own cargo feature. Claude Code and Copilot CLI installed through their own plugin CLIs; the other 23 harnesses got the plugin merged into their native config files. A backend ran only when the harness was present on the machine and supported the requested scope.
- Plugins installed from a blob baked into the binary at compile time, from a GitHub repo, or from a plugin tree on disk.
- A `build.rs` guard failed the build when `plugin.json` and `CARGO_PKG_VERSION` disagreed.
