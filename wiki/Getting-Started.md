# Getting started

## 1. Add the crate

The derive ships behind the default `derive` feature, so a consumer adds one dependency in two places (a build dependency is needed for the version guard).

```toml
[dependencies]
agentgear = "0.1"

[build-dependencies]
agentgear = "0.1"
```

> [!NOTE]
> `0.1` resolves once `0.1.0` ships. During the release candidate, pin the prerelease
> explicitly: `cargo add agentgear@0.1.0-rc.1` (add `--build` for the build-dependency).

## 2. Lay out the plugin tree

The binary embeds a plugin tree at compile time. The default location is `plugin/` next to `Cargo.toml`, in the single-plugin root layout:

```text
mytool/
  Cargo.toml
  build.rs
  plugin/
    .claude-plugin/
      plugin.json
  src/
    main.rs
```

`plugin.json` is the only file the tree needs:

```json
{
  "name": "mytool",
  "version": "0.1.0",
  "description": "What the plugin does",
  "author": { "name": "you" }
}
```

Its `name` must match the derive's `name` attr, and its `version` must equal `CARGO_PKG_VERSION`. The crate generates `marketplace.json`, so you do not ship one. Component dirs (`commands/`, `agents/`, `skills/`, `hooks/`), MCP servers, and what each harness does with them: [Plugin tree](Plugin-Tree).

## 3. Derive the host

```rust
use agentgear::{PluginHost, Scope, Source};

#[derive(PluginHost)]
#[plugin(name = "mytool", agents = ["claude"])]
struct MyHost;
```

Attributes:

| attr | default | meaning |
|---|---|---|
| `name` | required | plugin name; the derive cross-checks it against `plugin.json` at compile time |
| `marketplace` | `= name` | marketplace name (the id becomes `name@marketplace`) |
| `version` | `env!("CARGO_PKG_VERSION")` | the plugin version constant |
| `tree` | `$CARGO_MANIFEST_DIR/plugin` | embedded tree path |
| `default_source` | `"embedded"` | `"embedded"` or `"github"` |
| `github_repo` | required for github | `"owner/repo"` |
| `agents` | `["claude"]` | which backends `setup` wires; an empty list is rejected at compile time |
| `embed` | `true` | bake the compressed tree in via `include_bytes!`; set `false` (with `default-features = false` on the crate) for a `default_source = "github"` host that ships no baked tree |
| `instructions_fn` | none | a path to a `fn() -> Option<String>`, called from `PluginHost::instructions`. Host-authored always-loaded guidance, delivered through each harness's native context channel. `opencode` is the only backend that writes it today (a `<plugin>-instructions.md` registered in `opencode.json`'s `instructions[]`); see [Instructions](Capability-Instructions) |
| `statusline_fn` | none | a path to a `fn() -> Option<StatusLineDecl>`, called from `PluginHost::statusline`. The status line the host owns, written into the harness's own single slot, stashing and restoring whatever value it displaces. `claude`, `qwen-code`, `antigravity-cli`, and `droid` write it today; see [Status line](Capability-Status-Line) |

> [!IMPORTANT]
> Every id in `agents = [...]` needs its cargo feature enabled on the `agentgear` dependency. The feature name is the id (`qwen-code`, `vscode-copilot`), and `claude` is on by default:
>
> ```toml
> [dependencies]
> agentgear = { version = "0.1", features = ["codex", "cursor"] }
> ```
>
> `all-agents` enables all 25 backends at once.

The derive is the whole customization seam: the attributes above plus `install_into` (step 5) to pick a subset of agents. No attribute reshapes one backend (a different MCP command for codex, or skipping hooks on cursor). For control past the attributes, hand-write `impl PluginHost` instead of deriving; you supply the five consts and `embedded_blob()` and give up the build guard and the baked tree, so it suits a `Source::Path`/`Source::GitHub` host. [Agent backends](Agent-Backends#customization-ceiling) has the detail.

## 4. Add the build guard

```rust
// build.rs
fn main() {
    agentgear::build::assert_plugin_version();
}
```

It fails the build when `plugin.json` and `CARGO_PKG_VERSION` disagree, tracks the tree for rebuilds, and emits the guard that turns a missing `build.rs` into a compile error. A custom `tree` attr needs `assert_plugin_version_at(<same dir>)`.

## 5. Wire the subcommands and the hook

```rust
fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("setup")         => { MyHost::install(Scope::User, Source::Embedded)?; }
        Some("update")        => { MyHost::update(Scope::User)?; }
        Some("uninstall")     => { MyHost::uninstall(Scope::User)?; }
        Some("self-heal")     => { MyHost::self_heal()?; }
        Some("check-restart") => {
            // UserPromptSubmit hook entry: print the reload notice if an update
            // landed that this session has not loaded yet.
            if let Some(notice) = MyHost::restart_pending() {
                println!("{notice}");
            }
        }
        Some("doctor")        => { print!("{}", MyHost::doctor()?); }
        _ => {}
    }
    Ok(())
}
```

`install` takes any `Source`: `Source::Embedded` decompresses the baked blob, `Source::Path(dir)` materializes an on-disk tree, `Source::GitHub { repo, ref_ }` installs from a GitHub marketplace pinned to `ref_` (sent to the CLI as `{repo}@{ref_}`; the default `ref_` is the `v{version}` tag). `self_heal`/`update`/`doctor` rehydrate each agent's own install source from its stamp marker. A `Source::Path` install stays path-sourced across repair. An explicit `Source::Embedded` install stays embedded even on a `default_source = "github"` host. An agent with no marker falls back to the `default_source` attr.

To target a subset of `AGENTS` (a `setup --agent gemini` flag), call `install_into` instead. An empty slice means all of them:

```rust
MyHost::install_into(Scope::User, Source::Embedded, &["gemini"])?;
```

`examples/multi-installer` builds its whole picker on this plus `agentgear::backend_for`.
`agentgear::AGENT_IDS` lists every backend id compiled into the build (each resolvable through
`backend_for`), for a host that wants the full roster rather than its own `AGENTS` subset.

The scope-taking methods (`install`, `install_into`, `update`, `uninstall`) accept `Scope::Project { path }`, which writes into a repo's own config instead of the user's home:

```rust
MyHost::install(Scope::Project { path: std::env::current_dir()? }, Source::Embedded)?;
```

Scope support is per backend. One whose `capabilities().scopes` omits `"project"` is skipped silently, and `vscode-copilot` is project-only (user scope skips it the same way). [Scopes & gates](Capability-Scopes) has the per-backend split and the trust gates.

Point the plugin's hooks at subcommands. The `SessionStart` hook calls `MyHost::self_heal()`, a no-op on a healthy install that repairs a broken one without resurrecting an uninstall. The `UserPromptSubmit` hook calls a `check-restart` subcommand wrapping `MyHost::restart_pending()`: after an out-of-band `setup update`, it prints a notice that the running session still has the old plugin loaded and needs a `/reload-plugins`. Claude Code does not hot-reload plugin hooks, so ship the `UserPromptSubmit` hook from your first release. It fires from whatever version the running session already has. Both hooks live in `hooks/hooks.json` at the plugin root:

```json
{
  "hooks": {
    "SessionStart":     [{ "hooks": [{ "type": "command", "command": "mytool self-heal" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "mytool check-restart" }] }]
  }
}
```

## 6. First run

```console
$ mytool setup
Installed
```

The first install is always explicit: the hook that triggers self-heal ships inside the plugin, so it cannot fire until the plugin is installed. See [How It Works](How-It-Works) for what runs under the hood and [Doctor](Doctor) for verifying an install.

Five runnable hosts live in the repo's [`examples/`](https://github.com/uwuclxdy/agentgear/tree/HEAD/examples):

- `hello-mcp`: the smallest working host, one MCP server to Claude Code only. Its tree is a lone `plugin.json`, and it carries the `instructions_fn` wiring.
- `kitchen-sink`: every component type (MCP server, hooks, command, subagent, skill) across seven harnesses.
- `multi-installer`: builds its own agent picker through `backend_for` + `install_into`, then installs a filtered subset.
- `hooks-everywhere`: four hook events fanned across 14 harnesses; its README carries the event map.
- `from-github`: a zero-embed host (`embed = false`, `default_source = "github"`) that tracks a remote repo.
