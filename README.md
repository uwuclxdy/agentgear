<div align="center">

# agentgear

**Ship a coding-agent plugin straight from your Rust binary: Claude Code plus 24 other coding agents.** One `setup` command installs into every agent it detects; a SessionStart hook self-heals the install after a version bump.

Rust library and derive macro for shipping a coding-agent plugin from a binary. For Claude Code it orchestrates the `claude plugin` CLI and never forges its on-disk registry state. For the other 24 agents it read-modify-writes each tool's own config file, touching only the entries it wrote.

[![ci](https://shields.uwuclxdy.dev/github/actions/workflow/status/uwuclxdy/agentgear/ci.yml?label=ci)](https://github.com/uwuclxdy/agentgear/actions/workflows/ci.yml)
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

- **One binary, 25 agents.** `agents = [...]` in the derive picks the targets. Claude Code gets the full plugin lifecycle; the rest get config-merge: mcp servers, hooks, commands, agent defs translated into each tool's own config file. Only installed tools are touched.
- **One dependency, one derive.** Add the crate, write a `#[derive(PluginHost)]` struct and a one-line `build.rs`. The binary then gets `install` / `update` / `uninstall` / `self_heal` / `doctor`.
- **The CLI is the source of truth.** Every mutation goes through `claude plugin …`, so a Claude Code registry schema bump never breaks the crate. It reads state back through `list --json` for drift checks.
- **Self-heal that respects the user.** A SessionStart hook repairs a broken install without overriding a deliberate choice: it never resurrects an uninstall, re-enables a disable, or downgrades a newer install.
- **Tells the model when to reload.** After an out-of-band `setup update`, a `UserPromptSubmit` hook surfaces a restart-pending flag so the model tells the user to run `/reload-plugins`; the next `self_heal` clears it once the new version is loaded.
- **Atomic materialize.** The plugin tree ships as a compressed blob (a pure-Rust brotli archive, roughly a quarter of the raw text size). It decompresses into a content-keyed versioned directory with an atomic pointer flip, so a crash mid-install leaves the previous state intact.
- **Compile-time version guard.** A `build.rs` helper fails the build when `plugin.json` and `CARGO_PKG_VERSION` disagree, because Claude Code caches on the plugin version and a no-bump change is a silent no-op.

## How it works

`install` acquires a shared lock, then converges the `claude plugin` registry to the embedded plugin version:

| step | action |
|---|---|
| materialize | write the baked tree to `~/.local/share/<name>/versions/<ver>/`, generate `marketplace.json`, flip the atomic `current` pointer |
| register | `claude plugin marketplace add <current>` (Claude Code copies it into its cache) |
| install | `claude plugin install <name>@<marketplace>`, verified via `list --json` |
| stamp | record a marker so `self_heal` can tell an install it owns from one it should leave alone |

`self_heal` runs from the plugin's own SessionStart hook and reduces to the same reconcile, driven by the stamp marker and `list --json` state. Full state table: [How it works](https://github.com/uwuclxdy/agentgear/wiki/How-It-Works).

The 24 config-merge agents skip the marketplace steps: their `reconcile` read-modify-writes the tool's own config file instead. See [Supported agents](#supported-agents).

## Install

> [!NOTE]
> Pre-release. The crate is not on crates.io yet; depend on it by git until the first tagged release.

```toml
[dependencies]
agentgear = { git = "https://github.com/uwuclxdy/agentgear" }

[build-dependencies]
agentgear = { git = "https://github.com/uwuclxdy/agentgear" }
```

The derive ships with the crate behind the default `derive` feature, so consumers add one dependency.

## Usage

Point the derive at the plugin tree your binary embeds and add the build guard:

```rust
use agentgear::{PluginHost, Scope, Source};

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
    agentgear::build::assert_plugin_version();
}
```

Wire the plugin's SessionStart hook to call `self_heal` so an existing install repairs itself each session:

```rust
MyHost::self_heal()?; // no-op on a healthy install
```

The plugin tree lives at `<crate>/plugin/.claude-plugin/plugin.json` by default, with its `version` equal to `CARGO_PKG_VERSION`.

The Claude Code backend needs `claude` ≥ 2.1.196 on PATH at runtime; `setup` fails the version gate with a clear error below that. The config-merge backends have no CLI requirement.

## Examples

Two runnable hosts live in [`examples/`](examples/), both workspace members with tests that run in plain `cargo test`:

- [`hello-mcp`](examples/hello-mcp): the smallest real host. One derive, a one-line `build.rs`, a `setup` subcommand that ships one MCP server to Claude Code.
- [`kitchen-sink`](examples/kitchen-sink): every component type (MCP server, hooks, command, subagent, skill) across seven harnesses, plus its own dependency-free stdio MCP server and hermetic lifecycle tests.

## Feature flags

| flag | default | effect |
|---|---|---|
| `derive` | on | re-exports `#[derive(PluginHost)]` |
| `claude` | on | the Claude Code backend |
| `embed` | on | bakes the plugin tree into the binary as a compressed blob; turn off (with `embed = false` on the derive) for a `default_source = "github"` host that tracks a remote repo and ships no baked tree |
| `codex` | off | the codex backend (pulls in `toml_edit`) |
| one per agent | off | a feature per config-merge backend (24 total); `kimi` also pulls `toml_edit`, `goose` pulls `serde_norway` |
| `all-agents` | off | every backend above, enabled at once |

## Supported agents

Every id in the derive's `agents = [...]` list gets its own backend. Claude Code runs the full plugin lifecycle described above; the other 24 read-modify-write the target tool's own config file, translating what the plugin declares into that tool's shape. A backend only writes when it detects the tool installed. It touches its own entries only, so `uninstall` removes exactly what agentgear wrote.

Grouped by what each translates:

| translated surfaces | agents |
|---|---|
| plugin lifecycle (mcp, hooks, commands, agents, skills) | `claude` |
| mcp, hooks, commands, agents | `codex`, `cursor`, `devin`, `qwen-code`, `droid`, `augment` |
| mcp, hooks, agents | `copilot-cli`, `vscode-copilot` |
| mcp, hooks, commands | `gemini`, `cline` |
| mcp, hooks | `kimi`, `kiro`, `antigravity-cli`, `goose`, `crush` |
| mcp, commands, agents | `opencode`, `omp`, `kilo` |
| mcp only | `jetbrains-copilot`, `zed`, `openclaw`, `antigravity`, `amp` |
| detect-only, no surface | `pi` |

No backend translates skills yet. `vscode-copilot` writes at project scope only; the rest are user-scope-primary. Codex's hooks are written but stay inert until a user trusts them in codex's `/hooks` TUI; kimi's fire as soon as they are written. Per-agent config paths and skipped-surface reasons are on the [Agent backends](https://github.com/uwuclxdy/agentgear/wiki/Agent-Backends) wiki page.

## Status

25 agent backends ship: Claude Code (full plugin lifecycle) plus 24 config-merge backends. The original six (codex, opencode, gemini, cursor, cline, devin) are verified against their real CLIs: a per-tool docker leg installs the tool, runs `setup`, confirms the plugin's MCP server through the tool's own `mcp list`, then checks `uninstall` strips exactly what agentgear wrote while a seeded foreign entry survives. All six pass, native `mcp list` included. The 18 newer backends are covered by hermetic config-file tests plus unit tests (green locally); their docker legs (13, since GUI/IDE and no-surface backends have none) are authored and first run on CI push. The `AgentBackend` trait is unsealed, so an external crate can add an agent this crate does not ship. Linux is CI-gated, macOS is tested, Windows is designed in but not gated in CI: its pointer flip uses a directory junction, and unlike the posix rename it is delete-then-create, so a crash inside that window leaves `current` absent until the next materialize repairs it.

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
Yes, 25 in total. List the ids you want in the derive, e.g. `agents = ["claude", "codex", "cursor"]`; `setup` installs into every one it detects. See [Supported agents](#supported-agents) for the full roster and what each translates.

**Do I need to publish the plugin to a marketplace?**
No. In embedded mode the plugin tree is baked into the binary as a compressed blob and served from a locally generated marketplace. A GitHub source mode is available when you want `claude plugin update` to pull plugin changes without a binary release. A `Source::Path` mode installs from an on-disk tree at runtime; the recurring `self_heal`/`update`/`doctor` still resolve against the derive's `default_source`, so a self-healing host keeps an `embedded` or `github` default.

## Documentation

The README is a map. The reference lives in the wiki.

| page | topic |
|---|---|
| [Getting started](https://github.com/uwuclxdy/agentgear/wiki/Getting-Started) | add the crate, derive, build guard, hook wiring |
| [How it works](https://github.com/uwuclxdy/agentgear/wiki/How-It-Works) | lifecycle to CLI mapping, materialize, the self-heal state table |
| [Agent backends](https://github.com/uwuclxdy/agentgear/wiki/Agent-Backends) | the unsealed trait, the 24 config-merge backends, adding your own |
| [Doctor](https://github.com/uwuclxdy/agentgear/wiki/Doctor) | the six health checks and their fix hints |

## Development

```sh
cargo test                                          # unit + hermetic backend tests
cargo test -- --ignored                             # Claude Code e2e (needs `claude` on PATH)
cargo clippy --all-targets --all-features -- -D warnings
crates/host-fixture/tests/docker/run.sh <harness>   # one backend vs its real CLI (needs Docker + buildx)
```

## License

Licensed under MIT OR Apache-2.0, at your option.
