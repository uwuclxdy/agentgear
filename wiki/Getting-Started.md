# Getting started

## 1. Add the crate

The derive ships behind the default `derive` feature, so a consumer adds one dependency in two places (a build dependency is needed for the version guard).

```toml
[dependencies]
agentgear = { git = "https://github.com/uwuclxdy/agentgear" }

[build-dependencies]
agentgear = { git = "https://github.com/uwuclxdy/agentgear" }
```

## 2. Lay out the plugin tree

The binary embeds a plugin tree at compile time. The default location is `plugin/` next to `Cargo.toml`, in the single-plugin root layout:

```text
mytool/
  Cargo.toml
  build.rs
  plugin/
    .claude-plugin/
      plugin.json        # name + version + description + author
    commands/            # your commands, hooks, agents, skills
  src/
    main.rs
```

`plugin.json` `version` must equal `CARGO_PKG_VERSION`. The crate generates `marketplace.json`, so you do not ship one.

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
| `agents` | `["claude"]` | which backends `setup` wires |
| `embed` | `true` | bake the compressed tree in via `include_bytes!`; set `false` (with `default-features = false` on the crate) for a `default_source = "github"` host that ships no baked tree |

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

`install` takes any `Source`: `Source::Embedded` decompresses the baked blob, `Source::Path(dir)` materializes an on-disk tree, `Source::GitHub { repo, .. }` installs from a GitHub repo (today it tracks that repo's default branch — `ref_` is carried but not yet passed to the CLI). The recurring `self_heal`/`update`/`doctor` resolve against the `default_source` attr (which is `embedded` or `github`, never a runtime path), so `Source::Path` is a one-off install source rather than a host's steady state.

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

Five runnable hosts live in the repo's [`examples/`](https://github.com/uwuclxdy/agentgear/tree/mommy/examples):

- `hello-mcp`: this page in ~30 lines, one MCP server to Claude Code only.
- `kitchen-sink`: every component type (MCP server, hooks, command, subagent, skill) across seven harnesses.
- `multi-installer`: builds its own agent picker through `backend_for`, then installs a filtered subset.
- `hooks-everywhere`: four hook events fanned across the 15 hook-capable harnesses; its README carries the event map.
- `from-github`: a zero-embed host (`embed = false`, `default_source = "github"`) that tracks a remote repo.
