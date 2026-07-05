<div align="center">

# ez-agent-plugin

**Ship a Claude Code plugin straight from your Rust binary.** One `setup` command replaces `/plugin marketplace add` + `/plugin install`; a SessionStart hook self-heals the install across upgrades.

Rust library and derive macro for shipping a coding-agent plugin from a binary. It orchestrates the supported `claude plugin` CLI as its transaction boundary and never forges Claude Code's on-disk registry state.

[![ci](https://shields.uwuclxdy.dev/github/actions/workflow/status/uwuclxdy/ez-agent-plugin/ci.yml?label=ci)](https://github.com/uwuclxdy/ez-agent-plugin/actions/workflows/ci.yml)
[![license](https://shields.uwuclxdy.dev/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![claude code](https://shields.uwuclxdy.dev/badge/Claude%20Code-plugin%20lifecycle-8A2BE2)](https://code.claude.com/docs/en/plugins-reference)

</div>

Five Rust tools that each ship a Claude Code plugin were hand-rolling the same lifecycle: layout, version stamping, marketplace registration, self-heal, uninstall. Each drifted. This crate solves the hard parts once (atomic on-disk state, partial-failure recovery, schema-bump survival, concurrency) behind a derive macro.

```console
$ mytool setup
Installed
$ mytool doctor
[ ok ] host binary on PATH: `mytool` resolves on PATH
[ ok ] claude version: 2.1.201 (Claude Code)
[ ok ] plugin registered: mytool@mytool v0.4.0 (enabled)
[ ok ] manifest validates: ~/.local/share/mytool/current --strict clean
[ ok ] current tree matches embedded: hashes match
[ ok ] hook commands on PATH: all referenced bare commands resolve
```

## Why

- **One dependency, one derive.** Add the crate, write a `#[derive(PluginHost)]` struct and a one-line `build.rs`. The binary then gets `install` / `update` / `uninstall` / `self_heal` / `doctor`.
- **The CLI is the source of truth.** Every mutation goes through `claude plugin …`, so a Claude Code registry schema bump never breaks the crate. It reads state back through `list --json` for drift checks.
- **Self-heal that respects the user.** A SessionStart hook repairs a broken install without overriding a deliberate choice: it never resurrects an uninstall, re-enables a disable, or downgrades a newer install.
- **Atomic materialize.** The embedded plugin tree lands in a content-keyed versioned directory with an atomic pointer flip, so a crash mid-install leaves the previous state intact.
- **Compile-time version guard.** A `build.rs` helper fails the build when `plugin.json` and `CARGO_PKG_VERSION` disagree, because Claude Code caches on the plugin version and a no-bump change is a silent no-op.

## How it works

`install` acquires a shared lock, then converges the `claude plugin` registry to the embedded plugin version:

| step | action |
|---|---|
| materialize | write the baked tree to `~/.local/share/<name>/versions/<ver>/`, generate `marketplace.json`, flip the atomic `current` pointer |
| register | `claude plugin marketplace add <current>` (Claude Code copies it into its cache) |
| install | `claude plugin install <name>@<marketplace>`, verified via `list --json` |
| stamp | record a marker so `self_heal` can tell an install it owns from one it should leave alone |

`self_heal` runs from the plugin's own SessionStart hook and reduces to the same reconcile, driven by the stamp marker and `list --json` state. Full state table: [How it works](https://github.com/uwuclxdy/ez-agent-plugin/wiki/How-It-Works).

## Install

> [!NOTE]
> Pre-release. The crate is not on crates.io yet; depend on it by git until the first tagged release.

```toml
[dependencies]
ez-agent-plugin = { git = "https://github.com/uwuclxdy/ez-agent-plugin" }

[build-dependencies]
ez-agent-plugin = { git = "https://github.com/uwuclxdy/ez-agent-plugin" }
```

The derive ships with the crate behind the default `derive` feature, so consumers add one dependency.

## Usage

Point the derive at the plugin tree your binary embeds and add the build guard:

```rust
use ez_agent_plugin::{PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(name = "mytool", agents = ["claude"])]
struct MyHost;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("setup")   => { MyHost::install(Scope::User, Source::Embedded)?; }
        Some("doctor")  => { print!("{}", MyHost::doctor()?); }
        _ => {}
    }
    Ok(())
}
```

```rust
// build.rs
fn main() {
    ez_agent_plugin::build::assert_plugin_version();
}
```

Wire the plugin's SessionStart hook to call `self_heal` so an existing install repairs itself each session:

```rust
MyHost::self_heal()?; // no-op on a healthy install
```

The plugin tree lives at `<crate>/plugin/.claude-plugin/plugin.json` by default, with its `version` equal to `CARGO_PKG_VERSION`.

## Feature flags

| flag | default | effect |
|---|---|---|
| `derive` | on | re-exports `#[derive(PluginHost)]` |
| `claude` | on | the Claude Code backend |
| `codex`, `opencode` | off | reserved backend seams, unimplemented in v1 |

## Status

v1 targets Claude Code. The `AgentBackend` trait is a sealed seam for other coding agents; only the Claude Code backend is implemented. The trait stays sealed until a second backend lands from real use. Linux and macOS are the tested platforms; Windows support is designed in (directory junctions) but not gated in CI.

## Alternatives

Nothing else installs and repairs a Claude Code plugin from a host binary today. Here is how the crate compares to the manual routes.

| approach | the gap it leaves |
|---|---|
| Manual `/plugin marketplace add` + `/plugin install` | every user runs it by hand on every machine; nothing repairs a broken install after an upgrade |
| Shell install script wrapping the `claude plugin` CLI | re-runs blind with no version-keyed cache or atomic on-disk state; nothing repairs a later upgrade that breaks the install |
| Hand-rolled Rust in each repo (the status quo this replaces) | layout, version stamping, register, uninstall, self-heal all duplicated per repo; atomic state and partial-failure recovery solved nowhere |
| Anthropic team auto-install (`.claude/settings.json` `extraKnownMarketplaces` + `enabledPlugins`) | project-scoped and gated on a trust prompt; no host-binary control and no self-repair when an install breaks |

This crate collapses all four into one derive and a one-line `build.rs`.

## FAQ

**How do I install a Claude Code plugin from my own binary?**
Add the crate, derive `PluginHost`, wire a `setup` subcommand to `install()`. Running `mytool setup` performs `claude plugin marketplace add` and `install` for the user.

**How do I ship a Claude Code plugin without users running `/plugin marketplace add`?**
The embedded plugin tree materializes locally and registers itself through the `claude plugin` CLI during `setup`. Users never type the marketplace or install commands.

**Why does my Claude Code plugin keep reinstalling every session?**
A SessionStart hook that reinstalls unconditionally will resurrect a plugin the user disabled or removed. `self_heal` reads install state first. It repairs a broken install and leaves a deliberate uninstall or disable untouched.

**Will the plugin stay installed after I ship a new binary version?**
Yes. The SessionStart hook calls `self_heal`, which re-registers a plugin whose files went missing or stale after an upgrade. It never downgrades an install that is already newer.

**Does it work with coding agents other than Claude Code?**
Not yet. The `AgentBackend` trait is the seam for other agents such as codex or opencode. Only the Claude Code backend ships in v1; the trait stays sealed until a second backend lands from real use.

**Do I need to publish the plugin to a marketplace?**
No. In embedded mode the plugin tree is baked into the binary with `include_dir!` and served from a locally generated marketplace. A GitHub source mode is available when you want `claude plugin update` to pull plugin changes without a binary release.

## Documentation

The README is a map. The reference lives in the wiki.

| page | topic |
|---|---|
| [Getting started](https://github.com/uwuclxdy/ez-agent-plugin/wiki/Getting-Started) | add the crate, derive, build guard, hook wiring |
| [How it works](https://github.com/uwuclxdy/ez-agent-plugin/wiki/How-It-Works) | lifecycle to CLI mapping, materialize, the self-heal state table |
| [Agent backends](https://github.com/uwuclxdy/ez-agent-plugin/wiki/Agent-Backends) | the sealed trait and how a second agent would plug in |
| [Doctor](https://github.com/uwuclxdy/ez-agent-plugin/wiki/Doctor) | the six health checks and their fix hints |

## Development

```sh
cargo test                       # unit tests
cargo test -- --ignored          # end-to-end suite (needs `claude` on PATH)
cargo clippy --all-targets --all-features -- -D warnings
```

## License

Licensed under MIT OR Apache-2.0, at your option.
