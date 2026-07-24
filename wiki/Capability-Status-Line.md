# Status line

A status line is the bar a coding agent renders under (or over) its prompt, filled by running a
command and printing its stdout. A host declares one through `PluginHost::statusline` (the derive
exposes it as `statusline_fn`), returning a `StatusLineDecl { command, padding }`. Like
[instructions](Capability-Instructions), and unlike the five tree surfaces, it is not a file in the
plugin tree — it is a declaration the host returns at runtime.

**1 backend delivers it today.**

| harness | how it arrives |
|---|---|
| claude | the `statusLine` key in `<config>/settings.json` (user) or `<project>/.claude/settings.json`, as `{"type":"command","command":…}` plus `padding` when declared |

Four other harnesses (`qwen-code`, `antigravity-cli`, `droid`, `copilot-cli`) have a
command-based slot of their own; none is wired yet. The remaining twenty have no slot a host can
own — where they look like they do, the setting is a curated list of built-in widget ids, not a
command.

## The slot holds one value

This is the one surface agentgear cannot merge into. Every other write keys on names the plugin
owns, so a plugin's entries sit beside the user's. A status-line slot is a single value and
last-writer-wins, so writing it necessarily displaces whatever the user had. agentgear handles
that by stashing, not by refusing:

- **Install** copies the existing value into agentgear's own marker file, verbatim, before writing
  the host's command. An empty slot stashes nothing.
- **Uninstall** restores the stash exactly, or deletes the key when there was nothing to restore.
  A slot whose command is no longer the host's is left untouched: someone else owns it now.
- **Ownership is the command string.** Edit the `padding` on the host's line and the next
  `self_heal` puts it back (that is drift); replace the command and agentgear treats the slot as
  yours and stops touching it.
- Two agentgear hosts that both declare a status line will stack: the second stashes the first's
  command as "the original". Uninstalling both restores the first host's line, not what you had
  before either.

## Composing with the line you already had

agentgear writes the host's command and nothing else — it never generates a wrapper script. The
merge happens inside the host binary, which is what `agentgear::statusline` is for:

```rust
// in the host's own `statusline` subcommand, named by the declared command
let mut session = String::new();
std::io::stdin().read_to_string(&mut session)?;
let line = agentgear::statusline::compose(&MyHost::descriptor(), client, &session, &my_rows)?;
println!("{line}");
```

`compose` returns the host's own rows first, then runs the user's original command with the same
session JSON on its stdin and appends its rows (the surface is line-oriented: one line is one
row). A user command that cannot start, prints nothing, or runs past a 3-second timeout simply
contributes nothing, so a broken command of theirs never blanks the host's own bar.
`user_original` returns the stashed declaration directly if a host wants to render it itself.

Declare the command with [`${AGENTGEAR_CLIENT}`](Plugin-Tree#agentgear_client-per-harness-client-id)
(`mytool statusline --client ${AGENTGEAR_CLIENT}`) and each backend expands it to its own id, so
the host's subcommand knows which harness invoked it.

## See also

- [Capabilities](Capabilities) — the index.
- [Getting started](Getting-Started) — the `statusline_fn` derive attribute.
- [Doctor](Doctor) — the check that reports who owns the slot.
