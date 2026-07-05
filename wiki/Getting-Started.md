# Getting started

## 1. Add the crate

The derive ships behind the default `derive` feature, so a consumer adds one dependency in two places (a build dependency is needed for the version guard).

```toml
[dependencies]
ez-agent-plugin = { git = "https://github.com/uwuclxdy/ez-agent-plugin" }

[build-dependencies]
ez-agent-plugin = { git = "https://github.com/uwuclxdy/ez-agent-plugin" }
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
use ez_agent_plugin::{PluginHost, Scope, Source};

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

## 4. Add the build guard

```rust
// build.rs
fn main() {
    ez_agent_plugin::build::assert_plugin_version();
}
```

It fails the build when `plugin.json` and `CARGO_PKG_VERSION` disagree, tracks the tree for rebuilds, and emits the guard that turns a missing `build.rs` into a compile error. A custom `tree` attr needs `assert_plugin_version_at(<same dir>)`.

## 5. Wire the subcommands and the hook

```rust
fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("setup")     => { MyHost::install(Scope::User, Source::Embedded)?; }
        Some("update")    => { MyHost::update(Scope::User)?; }
        Some("uninstall") => { MyHost::uninstall(Scope::User)?; }
        Some("doctor")    => { print!("{}", MyHost::doctor()?); }
        _ => {}
    }
    Ok(())
}
```

Point the plugin's SessionStart hook at a subcommand that calls `MyHost::self_heal()`. It is a no-op on a healthy install and repairs a broken one. It never resurrects a plugin the user uninstalled.

## 6. First run

```console
$ mytool setup
Installed
```

The first install is always explicit: the hook that triggers self-heal ships inside the plugin, so it cannot fire until the plugin is installed. See [How It Works](How-It-Works) for what runs under the hood and [Doctor](Doctor) for verifying an install.
