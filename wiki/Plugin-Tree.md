# Plugin tree

The tree you ship is a stock Claude Code plugin. Anthropic owns that format: the [plugins reference](https://code.claude.com/docs/en/plugins-reference) has the full schema, the [plugins guide](https://code.claude.com/docs/en/plugins) has the concepts. This page covers only what agentgear adds on top or constrains further.

## Layout

The tree defaults to `plugin/` next to `Cargo.toml`. `.claude-plugin/plugin.json` is the only required file; every other directory is optional.

```text
mytool/
  Cargo.toml
  build.rs
  plugin/
    .claude-plugin/
      plugin.json
    .mcp.json            # optional, alternative to plugin.json mcpServers
    hooks/
      hooks.json
    commands/
      greet.md
    agents/
      reviewer.md
    skills/
      demo/
        SKILL.md
  src/
    main.rs
```

A different location needs the `tree` attr and a matching build guard:

```rust
#[plugin(name = "mytool", tree = "$CARGO_MANIFEST_DIR/assets/plugin")]
```

```rust
// build.rs
fn main() {
    agentgear::build::assert_plugin_version_at(
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/plugin"),
    );
}
```

`$CARGO_MANIFEST_DIR` is the one token the derive substitutes in `tree`; anything else is used as written.

## plugin.json

The real file from [`examples/hello-mcp`](https://github.com/uwuclxdy/agentgear/tree/HEAD/examples/hello-mcp):

```json
{
  "name": "hello-mcp",
  "version": "0.1.0",
  "description": "Minimal agentgear example plugin: ships one stdio MCP server to Claude Code.",
  "author": { "name": "agentgear examples" },
  "mcpServers": {
    "everything": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-everything"]
    }
  }
}
```

Five fields agentgear reads itself. The rest of the manifest is passed to Claude Code untouched.

| field | required by agentgear | what reads it |
|---|---|---|
| `name` | yes | the derive cross-checks it against the `name` attr at expansion; a mismatch or a missing/non-string value is a compile error |
| `version` | yes | the `build.rs` guard, compared against `CARGO_PKG_VERSION` |
| `author` | yes | becomes the generated marketplace `owner`. A bare string or an object with `name`/`email`/`url`. Missing, and install fails with an error naming it |
| `description` | no | the generated marketplace description. Absent, and it falls back to `"<name> plugin"` |
| `mcpServers` | no | parsed into the components IR that every non-Claude backend renders from |

`author` is stricter here than upstream: `claude plugin validate --strict` requires the marketplace `owner`, and agentgear has nowhere else to source it.

## Version lock

`plugin.json` `version` must equal `CARGO_PKG_VERSION`. The `build.rs` guard fails the build on a mismatch:

```text
agentgear: plugin.json version `0.1.0` != CARGO_PKG_VERSION `0.2.0`; bump Cargo.toml and plugin.json together (CC caches on version, so a mismatch ships a silent no-op)
```

Claude Code keys its plugin cache on that version, and its own `plugin update` reports "already at latest" for changed tree content under a version already installed. agentgear reinstalls in that case, so the content still lands; the version remains the pin for the release tag, a GitHub `ref`, and the never-downgrade comparison, which is what the guard protects. Bump both files in the same commit.

## What is parsed from where

The parser walks the flattened tree once and fills the five `PluginComponents` surfaces. Backends render whatever they support and skip the rest. Per-harness coverage is on [Harness comparison](Harness-Comparison).

| surface | read from | notes |
|---|---|---|
| mcp servers | `.claude-plugin/plugin.json` `mcpServers`, then `.mcp.json`, then `.claude-plugin/.mcp.json` | first occurrence of a server name wins, so `plugin.json` beats both files |
| hooks | `hooks/hooks.json` and `.claude-plugin/hooks.json` | both are read when both exist, and their bindings concatenate. Nothing dedups, so a hook declared in both files is registered twice by every backend |
| commands | `commands/**/*.md` | nested paths are kept; the name is the file stem |
| agents | `agents/**/*.md` | same shape as commands |
| skills | `skills/<name>/**` | the directory level is required. A loose `skills/foo.md` has no skill dir, so it is ignored |

Two shapes are accepted for the mcp surface that upstream does not spell out:

- A string `mcpServers` value in `plugin.json` is a tree-relative path to a `.mcp.json`, read in place of an inline object.
- A `.mcp.json` may wrap its servers under `mcpServers` or be a bare `{name: spec}` map at the document root.

`mcpServers` is the only manifest key the parser reads. Hooks, commands, agents and skills come from the fixed paths in the table and nowhere else, so a `plugin.json` key pointing one of them at a different location contributes nothing to the IR. Claude Code still finds those surfaces when it reads the tree itself; every config-merge backend loses them without a word. Keep the four directories where the table says they are.

Command and agent frontmatter is parsed as flat `key: value` lines plus block scalars (`key: |`, `key: >`). Nested maps and sequences are not parsed into the IR, though the raw file bytes are kept for backends that copy the document through.

## `${CLAUDE_PLUGIN_ROOT}` portability

`${CLAUDE_PLUGIN_ROOT}` expands inside Claude Code's plugin runtime. Nothing expands it in the config files agentgear merges into, so the token would reach the shell verbatim and all 23 config-merge backends **silently skip** an MCP server or a hook command carrying it. A few tools do expand the token in their own reading of a Claude Code plugin tree (cursor and VS Code Copilot both do), which is a separate path from the config file agentgear writes.

agentgear does not apply the skip to Claude Code or the Copilot CLI, since both consume the plugin tree through their own plugin CLI.

A plugin that installs clean and then does nothing outside Claude Code usually traces back to this.

```json
{
  "mcpServers": {
    "mytool": { "command": "${CLAUDE_PLUGIN_ROOT}/bin/mytool", "args": ["mcp"] }
  }
}
```

That server reaches Claude Code and no one else. Write a bare executable name instead, which is what `kitchen-sink` ships:

```json
{
  "mcpServers": {
    "mytool": { "command": "mytool", "args": ["mcp"] }
  }
}
```

The same holds for hook commands. `kitchen-sink` and `hooks-everywhere` both point their hooks at their own binary by name (`kitchen-sink self-heal`), which is what makes them portable across harnesses.

The check covers an MCP server's `command` and `args`, and a hook's `command`. A `${CLAUDE_PLUGIN_ROOT}` inside an MCP `env` value or a remote server's `url` passes through verbatim and reaches the harness config unexpanded.

### Noticing a skip

Nothing fails, so the signal is what [Doctor](Doctor) does not say. Its mcp check names the servers it registered:

```text
[ ok ] mcp server registered: mytool registered
```

When every server is non-portable, the same check still passes, with no server named:

```text
[ ok ] mcp server registered: no portable mcp servers to register
```

Hook checks follow the same shape where a backend has one, such as antigravity-cli's `hooks registered` reporting `no portable, mappable hooks to register`. A "no portable" detail on a plugin that declares servers or hooks means they were dropped on the portability rule.

## `${AGENTGEAR_CLIENT}` per-harness client id

`${AGENTGEAR_CLIENT}` is the counterpart to `${CLAUDE_PLUGIN_ROOT}` above: agentgear knows this
token too, but **expands** it instead of skipping it. Every backend substitutes it for its own
client id (`claude` → `claude`, `codex` → `codex`, `copilot-cli` → `copilot-cli`, …) in a hook's
`command` and an MCP server's `command`/`args`. Write the token once and each harness's rendered
config carries its own id — handy for a hook binary that needs to know which harness invoked it,
without hardcoding one harness's name:

```json
{
  "mcpServers": {
    "mytool": { "command": "mytool", "args": ["mcp", "--client", "${AGENTGEAR_CLIENT}"] }
  }
}
```

For the 23 config-merge backends, only a hook's `command` and an MCP server's `command`/`args`
expand the token — a matcher, an `env` value, or a markdown body are left verbatim. `claude` and
`copilot-cli` substitute it across the *whole* materialized tree instead, since those two copy the
tree verbatim rather than rendering each surface; the token can appear anywhere in those trees
(env values, markdown, …) and still expands.

## marketplace.json

You never ship one. Materialize generates `.claude-plugin/marketplace.json` into the versioned directory from `plugin.json`, mapping `author` to `owner` and `description` through, then points `marketplace add` at it. A `marketplace.json` present in your source tree never survives into the materialized tree; the generated file lands on top of it.

The generated file carries no `version` field, and it is excluded from tree hashing so a source tree and a materialized tree hash equal. See [How it works](How-It-Works) for the materialize pipeline.
