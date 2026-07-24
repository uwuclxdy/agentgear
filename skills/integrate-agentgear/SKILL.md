---
name: integrate-agentgear
description: >-
  Add agentgear to a Rust binary that ships a Claude Code plugin, or migrate a hand-rolled
  install / self-heal / per-harness config-merge engine onto agentgear's derive. Use when a
  binary should install its own CC plugin (replacing manual `/plugin marketplace add` +
  `/plugin install`), when a plugin needs to reach non-CC harnesses without hand-writing a
  translator per harness, or when deciding whether a given plugin is even a fit.
---

# integrate-agentgear

One `#[derive(PluginHost)]` struct gives a host the full install / uninstall / update /
self-heal / doctor lifecycle. agentgear drives the `claude plugin` CLI for those ops and bakes the
plugin tree into the binary. It writes that one Claude-Code plugin into 23 other harnesses'
native config files, detect-gated, never clobbering user config (`copilot-cli` is a 25th target,
orchestrated through its own plugin CLI rather than a config-file write). The host declares intent once;
agentgear delivers it per harness.

This skill is the process. The exhaustive API lives in the repo's `wiki/` and `docs/` (mapped at
the end); cite those rather than restating them.

## fit check (do this first)

Answer these before writing any code. A "no" changes the plan.

1. **Does one binary both ship and install the plugin?** agentgear's embed model assumes the
   `PluginHost` binary IS the artifact that carries and installs the plugin tree. A plugin whose
   binary is a separate, independently-versioned artifact the plugin locates at runtime (the way
   slopcat's plugin finds a `slopcat` on PATH) has no clean agentgear story yet. Track it against
   the companion-binary item in `docs/todo.md` before committing to a migration.
2. **The plugin's own binary distribution stays the host's job.** `Source` models where the
   plugin *tree* comes from, never where the host's compiled binary comes from. Keep any
   curl+checksum / PATH-resolution / version-GC installer the host already has; agentgear does not
   replace it.
3. **Which harnesses does it target?** A Claude-Code-only plugin needs only the default `claude`
   backend, so the 24-harness translate does nothing for it. That is fine: the win there is the
   automated lifecycle (setup / self-heal / doctor / version-guard), not translation. Only name
   extra `agents = [...]` when the plugin genuinely ships to those harnesses.
4. **Does it hand-roll CC registry state directly?** Writing `known_marketplaces.json` or
   `installed_plugins.json` by hand (raawr did) is the exact anti-pattern agentgear removes: the
   CLI is the only supported writer. Migrating that is a correctness upgrade, not just a line cut.

## greenfield: minimal setup

The smallest working host is Claude-only. Four things (shapes from `examples/hello-mcp`):

- **`Cargo.toml`**: agentgear as both a normal and a build dependency. Default features
  (`derive` + `claude` + `embed`) cover a Claude-only host:
  ```toml
  [dependencies]
  agentgear = "0.1"
  [build-dependencies]
  agentgear = "0.1"
  ```
- **`build.rs`**: one line. It reads `plugin.json`, guards the version, and (with `embed`)
  compresses the tree into the binary:
  ```rust
  fn main() { agentgear::build::assert_plugin_version(); }
  ```
  A missing `build.rs` is a compile error, not a silent loss (the derive checks for its marker).
- **the derive struct**, in the lib (not just `main.rs`) so tests can read the metadata without
  spawning the binary:
  ```rust
  use agentgear::PluginHost;
  #[derive(PluginHost)]
  #[plugin(name = "my-plugin")]
  pub struct MyPlugin;
  ```
- **the `plugin/` tree**, next to `Cargo.toml`. The only required file is
  `plugin/.claude-plugin/plugin.json`; its `name` must equal the derive's `name`, its `version`
  must equal `CARGO_PKG_VERSION`, and `author` is required (it becomes the marketplace owner).

Then wire the binary's own subcommands to the lifecycle methods (`setup` → `install`,
`uninstall`, `doctor`, and for a fuller surface `update`, `self-heal`, `check-restart`). Point the
plugin's `SessionStart` hook at `self_heal()` and `UserPromptSubmit` at `restart_pending()`.

The full attribute table, the six-subcommand `main.rs`, and the tree rules are in
`wiki/Getting-Started.md` and `wiki/Plugin-Tree.md`. Do not restate them here; read them.

## brownfield: migrating a hand-rolled installer

Adopt the derive first (greenfield steps), get one green `setup` / `doctor`, then delete the
plumbing it replaces. Delete in this order, verifying after each:

| hand-rolled thing | replaced by |
|---|---|
| marketplace-register + plugin-install code (shell or Rust), incl. any `known_marketplaces.json` write | `install(Scope::User, Source::Embedded)` |
| direct `claude plugin ...` shell calls / a custom CLI wrapper | agentgear's `ClaudeCli` (locate, env-scrub, non-interactive, tolerant `--json`); no host code shells out to `claude` |
| a committed `marketplace.json` | generated into the versioned dir at materialize; never ship one |
| a per-client config-merge engine (per-harness JSON/TOML/YAML writers, "which client do I support" descriptor files) | the built-in backends + `confedit`'s atomic read-modify-write; name harnesses in `agents = [...]` instead of writing translators |
| a hand-rolled tree materialize / diff-copy | agentgear's `materialize` (tree hashing, atomic versioned-dir flip) |
| manual `SessionStart` self-heal logic | point the existing hook at `self_heal()` |
| a hand-maintained CC-version floor check | agentgear gates on `claude --version` before any mutating call |

nyactx is the worked example: adopting the derive deleted ~1,100 net lines (20 per-client
descriptor files plus a parse-and-merge engine plus its tests), replaced by ~160 lines of
CLI glue. The migration commit is the reference for scope.

Grep the tree afterward for the deleted symbols and any surviving `Command::new("claude")` /
`known_marketplaces` / `marketplace.json` reference; a mechanical delete leaves stragglers.

## multi-harness

- `agents = ["claude", "codex", "opencode", ...]` on the derive sets the fan-out list every
  lifecycle op walks. The 25 valid ids (closed list, unknown id = compile error) are: `claude`,
  `codex`, `opencode`, `gemini`, `cursor`, `cline`, `devin`, `qwen-code`, `copilot-cli`,
  `vscode-copilot`, `jetbrains-copilot`, `kimi`, `kiro`, `zed`, `omp`, `openclaw`, `kilo`,
  `antigravity`, `antigravity-cli`, `pi`, `goose`, `amp`, `crush`, `droid`, `augment`.
- **Each non-CC backend is a cargo feature named after its id** (dashes kept). List every backend
  you name in `agents` under `features`, or use `all-agents`:
  ```toml
  agentgear = { version = "0.1", features = ["codex", "opencode", "gemini"] }
  ```
  A backend named in `agents` whose feature is off is a compile error in the host crate, with the
  exact `features = [...]` fix in the message. `claude` is always on.
- `install_into(scope, source, &["codex"])` narrows a single run to a subset of `AGENTS` at
  runtime (the `--agent <id>` pattern). `backend_for(id)` resolves one id to its backend for a
  status/picker view. `examples/multi-installer` is the reference.
- **`${AGENTGEAR_CLIENT}` expands to each harness's own id** (`claude`, `codex`, …) in a hook's
  `command` and an MCP server's `command`/`args`; `claude` and `copilot-cli` expand it across the
  whole materialized tree. Migrate any baked-in `--client claude-code` to the token: the literal
  rides through every backend unchanged and evaluates true on all of them, so a hook binary that
  branches on its caller sees the wrong harness. Unlike `${CLAUDE_PLUGIN_ROOT}` (skipped, below),
  this token is expanded, so a command carrying it stays portable.

Per-harness fidelity (what each backend translates, what it skips and why) is in
`docs/harness/<id>.md` and `docs/harness/matrix.md`. Read the target backend's doc before promising
a surface reaches it.

## what does NOT migrate yet (gap blockers)

Check these against the target plugin before promising a full migration. Each is an open item in
`docs/todo.md`; some in depth in `docs/fox-nyactx-integration.md`.

- **statusLine / any harness settings key outside the plugin tree.** The components IR models five
  surfaces (mcp servers, hooks, commands, agents, skills). A plugin that writes CC's `statusLine`
  into the user's `~/.claude/settings.json` (nyactx, ragcat) keeps hand-rolling that until the
  host-settings surface lands.
- **codex `notify` hooks / opencode hooks.** agentgear's codex backend writes `hooks.json`, which
  stays inert until the user trusts it via codex's `/hooks` TUI; opencode has no declarative hook
  surface at all (in-process JS/TS plugin only). A plugin relying on either (raawr) cannot fully
  migrate its hook path yet.
- **`${CLAUDE_PLUGIN_ROOT}` in an mcp/hook command.** Every non-CC backend silently skips a
  command carrying that token (it only expands inside CC). Use bare executable names so the surface
  survives translation. Watch for the `cargo:warning` at build and doctor's "no portable ... to
  register" line.

## verify

- `setup` then `doctor` on a scratch `HOME` install the plugin and report healthy; `uninstall`
  strips exactly what was written.
- The hand-rolled plumbing named in the migration table is gone (grep the deleted symbols).
- A `plugin.json` / `Cargo.toml` version mismatch fails the build (the guard, not a runtime error).
- For a multi-harness host, install into a scratch profile for each named backend and confirm its
  native config file carries the plugin, gated on `detect()`.
- Hermetic-test discipline (env overrides to clear, why `claude`/`copilot-cli` need an `#[ignore]`d
  real-CLI run) is in `wiki/Testing-Your-Host.md`.

## reference map

| need | read |
|---|---|
| every `#[plugin(...)]` attr, defaults, the tree rules | `wiki/Getting-Started.md`, `wiki/Plugin-Tree.md` |
| lifecycle method signatures, `Scope`, `Source`, reports | `wiki/Types-and-Errors.md`, `docs/design.md` (public API) |
| which backend translates what, event maps, skips | `wiki/Agent-Backends.md`, `docs/harness/<id>.md`, `docs/harness/matrix.md` |
| locked design decisions, accepted limits, ground-truth schemas | `docs/design.md` |
| open gaps + the nyactx adoption backlog | `docs/todo.md`, `docs/fox-nyactx-integration.md` |
| test harness rules | `wiki/Testing-Your-Host.md` |
