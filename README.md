<div align="center">

# agentgear

**Ship a coding-agent plugin straight from your Rust binary: Claude Code plus 24 other coding agents.** One `setup` command installs into every agent it detects; a SessionStart hook self-heals the install after a version bump.

Rust library and derive macro for shipping a coding-agent plugin from a binary. For Claude Code and GitHub Copilot CLI it orchestrates each tool's own plugin-management CLI and never forges its on-disk registry state. For the other 23 agents it read-modify-writes each tool's own config file, touching only the entries it wrote.

[![crates.io](https://shields.uwuclxdy.dev/crates/v/agentgear)](https://crates.io/crates/agentgear)
[![docs.rs](https://shields.uwuclxdy.dev/docsrs/agentgear)](https://docs.rs/agentgear)
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
[ ok ] marketplace registered: `mytool` registered
[ ok ] manifest validates: ~/.local/share/mytool/current@claude --strict clean
[ ok ] current tree matches embedded: hashes match
[ ok ] hook commands on PATH: all referenced bare commands resolve
```

## Why

- **One binary, 25 agents.** `agents = [...]` in the derive picks the targets. Claude Code and GitHub Copilot CLI get the full plugin lifecycle; the other 23 get config-merge: mcp servers, hooks, commands, agent defs translated into each tool's own config file. Only installed tools are touched.
- **One dependency, one derive.** Add the crate, write a `#[derive(PluginHost)]` struct and a one-line `build.rs`. The binary then gets `install` / `update` / `uninstall` / `self_heal` / `doctor`.
- **Know which agent did what.** Each lifecycle call has a `_report` twin (`install_report`, `update_report`, `uninstall_report`, `self_heal_report`) returning an `AgentReport`: one `AgentResult` per configured agent, each `Converged` (with its own `Outcome`), `Skipped` (with a `SkipReason`), or `Failed` (with the detail). A host reads it to tell the user which of its 25 agents actually installed instead of guessing from one collapsed result. The plain `install` / `update` / `uninstall` / `self_heal` calls still return a single merged `Outcome` for a host that only wants pass/fail.
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

The 23 config-merge agents skip the marketplace steps: their `reconcile` read-modify-writes the tool's own config file instead. GitHub Copilot CLI runs the same marketplace-add/install/update shape as Claude Code, against its own `copilot plugin` CLI. See [Supported agents](#supported-agents).

## Install

```toml
[dependencies]
agentgear = "0.1"

[build-dependencies]
agentgear = "0.1"
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

The Claude Code backend needs `claude` ≥ 2.1.196 on PATH at runtime; the copilot-cli backend needs `copilot` ≥ 1.0.71 (its `plugin` subcommand didn't exist before). `setup` fails the version gate with a clear error below that version. The config-merge backends have no CLI requirement.

## Customization

The seam is the derive: the `#[plugin(..)]` attributes plus `install_into` to target a subset of agents at runtime. Every knob configures the whole plugin. None reshapes one backend: nothing overrides how a backend renders (say a different MCP command for codex) or drops a surface for one backend (say skipping hooks on cursor). Each backend writes what its target tool supports, gated by detection and the `${CLAUDE_PLUGIN_ROOT}` portability filter.

`instructions_fn` and `statusline_fn` are the two method overrides, because the derive emits the sole `impl PluginHost` block. For control past the attributes, write `impl PluginHost` by hand instead of deriving. Supply the five consts (`NAME`, `MARKETPLACE`, `VERSION`, `DEFAULT_SOURCE`, `AGENTS`) and `embedded_blob()`; the lifecycle methods come with the trait. This drops the derive's compile-time guards (the missing-`build.rs` check and the `agents`-vs-feature check) and the baked `include_bytes!` tree, so a hand-written host returns `&[]` from `embedded_blob()` and installs from a `Source::Path` or `Source::GitHub`. See [Agent backends](https://github.com/uwuclxdy/agentgear/wiki/Agent-Backends) for the detail.

## Examples

Five runnable hosts live in [`examples/`](examples/), all workspace members with tests that run in plain `cargo test`:

- [`hello-mcp`](examples/hello-mcp): the smallest real host. One derive, a one-line `build.rs`, a `setup` subcommand that ships one MCP server to Claude Code.
- [`kitchen-sink`](examples/kitchen-sink): every component type (MCP server, hooks, command, subagent, skill) across seven harnesses, plus its own dependency-free stdio MCP server and hermetic lifecycle tests.
- [`multi-installer`](examples/multi-installer): builds its own agent picker by enumerating backends through `backend_for` (detected vs not), then installs into a filtered subset.
- [`hooks-everywhere`](examples/hooks-everywhere): four Claude Code hook events translated across 14 harnesses; its README carries the per-harness event map.
- [`from-github`](examples/from-github): a zero-embed host (`embed = false`, `default_source = "github"`) that tracks a remote repo instead of baking a tree.

## Feature flags

| flag | default | effect |
|---|---|---|
| `derive` | on | re-exports `#[derive(PluginHost)]` |
| `claude` | on | the Claude Code backend |
| `embed` | on | bakes the plugin tree into the binary as a compressed blob; turn off (with `embed = false` on the derive) for a `default_source = "github"` host that tracks a remote repo and ships no baked tree |
| `codex` | off | the codex backend (pulls in `toml_edit`) |
| one per agent | off | a feature per non-CC backend (24 total; `copilot-cli` is plugin-native, the rest config-merge); `kimi` pulls `toml_edit`, `goose` and `omp` pull `serde_norway` |
| `all-agents` | off | every backend above, enabled at once |

A feature name is the agent id, and the two lists must match: an id in `agents = [...]` whose feature is off fails the build, naming the id and the missing feature.

```toml
[dependencies]
agentgear = { version = "0.1", features = ["codex", "cursor", "opencode"] }
```

```rust
#[plugin(name = "mytool", agents = ["claude", "codex", "cursor", "opencode"])]
```

## Supported agents

Every id in the derive's `agents = [...]` list gets its own backend, gated by a cargo feature of the same name (see [Feature flags](#feature-flags)); a listed id with the feature off fails the build. Claude Code and copilot-cli run the full plugin lifecycle described above; the other 23 read-modify-write the target tool's own config file, translating what the plugin declares into that tool's shape. A backend only writes when it detects the tool installed. It touches its own entries only, so `uninstall` removes exactly what agentgear wrote.

Grouped by what each surface translates:

| translated surfaces | agents |
|---|---|
| plugin lifecycle (mcp, hooks, commands, agents, skills) | `claude`, `copilot-cli` |
| mcp, hooks, commands, agents, skills | `cursor`, `devin`, `qwen-code`, `droid` |
| mcp, hooks, commands, agents | `codex`, `gemini`, `augment` |
| mcp, hooks, commands, skills | `crush` |
| mcp, commands, agents, skills | `kilo` |
| mcp, hooks, agents | `vscode-copilot` |
| mcp, hooks, commands | `cline` |
| mcp, commands, agents, instructions | `opencode` |
| mcp, commands, agents | `omp` |
| mcp, hooks, skills | `kimi`, `goose` |
| mcp, hooks | `antigravity-cli` |
| mcp, skills | `kiro`, `zed`, `openclaw` |
| mcp only | `jetbrains-copilot`, `antigravity`, `amp` |
| detect-only, no surface | `pi` |

Skills translate on 13 of 25 backends now (see above). Two surfaces sit outside that grid because
the host declares them instead of shipping them in the tree: instructions (always-loaded guidance
from `PluginHost::instructions`) translate on `opencode` only so far, written to a dedicated file
whose path is registered in opencode's `instructions[]`; a status line
(`PluginHost::statusline`) lands on `claude`, `copilot-cli`, `qwen-code`, `antigravity-cli`, and
`droid`, written into each tool's own single status-line slot, where agentgear stashes whatever was
there and puts it back on uninstall. Of the 24 non-Claude backends, 14 accept both scopes and 9 are user-scope only (`copilot-cli` among them, its CLI has no `--scope`). `vscode-copilot` is the one project-scope-only backend. Codex's hooks are written but stay inert until a user trusts them in codex's `/hooks` TUI; kimi's fire as soon as they are written. Per-agent config paths and skipped-surface reasons are on the [Agent backends](https://github.com/uwuclxdy/agentgear/wiki/Agent-Backends) wiki page; the harness-first at-a-glance (config paths, scopes, support grid) is on [Harness comparison](https://github.com/uwuclxdy/agentgear/wiki/Harness-Comparison), and the capability-first depth — a page each for MCP shapes + fidelity, hook events, skills, native Claude-Code-config interop — is on [Capabilities](https://github.com/uwuclxdy/agentgear/wiki/Capabilities).

## Status

| area | state |
|---|---|
| backends | 25 ship: Claude Code + copilot-cli (full plugin lifecycle) plus 23 config-merge backends |
| real-tool verification | every non-CC backend re-verified against the real shipping tool on 2026-07-16: fresh scratch-home installs (shipped extension source for the IDE-bound ones), with positive and negative controls before trusting any parse |
| docker legs | 19 per-tool legs (GUI/IDE and no-surface backends have none): each installs the real tool, runs `setup`, checks the written config, then checks `uninstall` strips exactly what agentgear wrote and leaves a seeded foreign entry in place. All 19 pass; the CLI-testable legs assert through the tool's own `mcp list` |
| hermetic tests | every backend has config-file tests plus unit tests, green in plain `cargo test` |
| remote (http/sse) MCP | fidelity varies per tool: most read the rendered shape as-is, a few key the transport off other fields or reject the shape whole. Remote stays best-effort on non-CC backends until per-tool fixes land; stdio is the verified path everywhere |
| extensibility | the `AgentBackend` trait is genuinely implementable out of crate: `DoctorReport::from_checks`/`from_error` build a report from outside, and `Error::Backend` gives a foreign failure a neutral shape. The derive's `agents = [...]` still can't name an external id, so it reconciles beside the built-in fan-out via `Plugin::components()` plus a direct `reconcile`/`remove` call |
| platforms | Linux CI-gated, macOS tested. Windows is designed in but not CI-gated: its pointer flip uses a directory junction (delete-then-create, unlike the posix rename), so a crash inside that window leaves `current` absent until the next materialize repairs it |

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
No. In embedded mode the plugin tree is baked into the binary as a compressed blob and served from a locally generated marketplace. A GitHub source mode is available when you want `claude plugin update` to pull plugin changes without a binary release. A `Source::Path` mode installs from an on-disk tree at runtime; `self_heal`/`update`/`doctor` rehydrate that path from the install's own stamp marker, so it stays path-sourced across repair. `embedded` and `github` installs resolve against the derive's `default_source`.

## Versioning

| item | policy |
|---|---|
| MSRV | Rust 1.88, measured rather than the edition 2024 floor: the lib uses let-chains, which 1.87 rejects and 1.88 stabilized. Raising it ships as a minor bump |
| semver | pre-1.0, so a minor bump may break the API; patch releases stay compatible |
| changes | each tag's [GitHub Release](https://github.com/uwuclxdy/agentgear/releases) carries its own notes |

## Documentation

The README is a map. The reference lives in the wiki.

| page | topic |
|---|---|
| [Getting started](https://github.com/uwuclxdy/agentgear/wiki/Getting-Started) | add the crate, derive, build guard, hook wiring |
| [Plugin tree](https://github.com/uwuclxdy/agentgear/wiki/Plugin-Tree) | tree layout, `plugin.json`, the version lock, `${CLAUDE_PLUGIN_ROOT}` portability |
| [How it works](https://github.com/uwuclxdy/agentgear/wiki/How-It-Works) | lifecycle to CLI mapping, materialize, the self-heal state table |
| [Types and errors](https://github.com/uwuclxdy/agentgear/wiki/Types-and-Errors) | the programmatic API: `AgentReport`/`AgentResult`/`AgentStatus`/`SkipReason`, the `Error` enum, building a `DoctorReport` from outside the crate |
| [Agent backends](https://github.com/uwuclxdy/agentgear/wiki/Agent-Backends) | the unsealed trait, the 23 config-merge backends, adding your own |
| [Harness comparison](https://github.com/uwuclxdy/agentgear/wiki/Harness-Comparison) | harness-first at-a-glance: support grid, config paths, scopes |
| [Capabilities](https://github.com/uwuclxdy/agentgear/wiki/Capabilities) | capability-first depth: MCP, hooks, commands, agents, skills, instructions + detection, scopes, native ingestion |
| [Testing your host](https://github.com/uwuclxdy/agentgear/wiki/Testing-Your-Host) | hermetic lifecycle tests: env redirects, forcing detection, the shared lock, what they miss |
| [Doctor](https://github.com/uwuclxdy/agentgear/wiki/Doctor) | the health checks and their fix hints |

## Development

```sh
cargo test                                          # unit + hermetic backend tests
cargo test -- --ignored                             # Claude Code e2e (needs `claude` on PATH)
cargo clippy --all-targets --all-features -- -D warnings
crates/host-fixture/tests/docker/run.sh <harness>   # one backend vs its real CLI (needs Docker + buildx)
```

## License

Licensed under MIT OR Apache-2.0, at your option.
