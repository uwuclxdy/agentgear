# multi-installer

The setup-picker pattern: a host that enumerates every backend its derive declares
and lets the caller target any subset, instead of installing (or reporting on) all
25 at once. Demonstrates `agentgear::backend_for`, the public API a host uses to
build its own per-backend picker UI, and `install_into`'s `--agent` filter.

Ships a minimal plugin tree (one MCP server, one command) since the point is the
picker, not the tree; see [`kitchen-sink`](../kitchen-sink) for the full component
surface, or [`hello-mcp`](../hello-mcp) for the smallest host to start from.

The picker itself is the `status` arm of `src/main.rs`. `src/lib.rs` holds the derive
listing all 25 ids.

## Run it

```sh
cargo run -p multi-installer -- status
cargo run -p multi-installer -- setup --agent gemini   # one backend only
cargo run -p multi-installer -- setup                  # every detected backend
cargo run -p multi-installer -- uninstall
cargo run -p multi-installer -- self-heal
cargo run -p multi-installer -- doctor
cargo run -p multi-installer -- mcp                    # the plugin's own stdio server
```

`status` resolves every `MultiInstaller::AGENTS` id through `backend_for` and
prints its detection state plus what it can host, real output from a scratch
`HOME` with only `~/.gemini` and `CODEX_HOME` pre-created:

```console
$ multi-installer status
id                 detected mcp      hooks    plugins  commands agents   skills   scopes
claude             no       yes      yes      yes      yes      yes      yes      user, project
codex              yes      yes      yes      no       yes      yes      no       user, project
gemini             yes      yes      yes      no       yes      yes      no       user, project
cursor             no       yes      yes      no       yes      yes      yes      user, project
...
pi                 no       no       no       no       no       no       no       user
```

`setup --agent <id>` narrows the fan-out to that one backend, even when other
backends are detected on the machine too:

```console
$ multi-installer setup --agent gemini
Installed
```

## Tests

`tests/picker.rs` runs `multi-installer` against a temp `HOME` (+ `CODEX_HOME`,
+ XDG dirs), never the real user config:

- `status_lists_every_agent`: every `MultiInstaller::AGENTS` id gets a row.
- `setup_agent_filter_scopes_to_one_backend`: `setup --agent gemini` writes
  gemini's config (merge-safe: a seeded foreign mcp server and top-level key
  survive). codex's config stays byte-identical though codex is detected too.
  A second `setup` is a true `NoOp`; `uninstall` removes only what we wrote.

```sh
cargo test -p multi-installer
```
