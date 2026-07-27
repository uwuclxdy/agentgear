# Status line

A status line is the bar a coding agent renders under (or over) its prompt, filled by running a
command and printing its stdout. A host declares one through `PluginHost::statusline` (the derive
exposes it as `statusline_fn`), returning a `StatusLineDecl { command, padding }`. Like
[instructions](Capability-Instructions), and unlike the five tree surfaces, it is not a file in the
plugin tree — it is a declaration the host returns at runtime.

**5 backends deliver it today**, through one shared slot renderer.

| harness | how it arrives |
|---|---|
| claude | the `statusLine` key in `<config>/settings.json` (user) or `<project>/.claude/settings.json`, as `{"type":"command","command":…}` plus `padding` when declared |
| copilot-cli | the root `statusLine` key in `settings.json` under `$COPILOT_HOME` (else `~/.copilot`), **user scope only**. Same shape as Claude Code's; copilot's renderer reads `command` and `padding` |
| qwen-code | `ui.statusLine` in the same `settings.json` its other surfaces use, both scopes. `type: "command"` is required there: anything else is silently read as a built-in preset and the declared command never runs |
| antigravity-cli | the root `statusLine` key in `~/.gemini/antigravity-cli/settings.json`, **user scope only**. That harness has an on/off toggle of its own (`enabled`) which agentgear preserves rather than overrides |
| droid | the root `statusLine` key in `~/.factory/settings.json` (user) or `<project>/.factory/settings.json`, plus droid's own `maxRows` cap so a composed two-row line is not clipped to one |

The remaining twenty harnesses have no slot a host can own. Where they look like they do, the
setting is a curated list of built-in widget ids rather than a command.

## The slot holds one value

This is the one surface agentgear cannot merge into. Every other write keys on names the plugin
owns, so a plugin's entries sit beside the user's. A status-line slot is a single value and
last-writer-wins, so writing it necessarily displaces whatever the user had. agentgear handles
that by stashing, not by refusing:

- **Install** copies the existing value into agentgear's own marker file, verbatim, before writing
  the host's command. An empty slot stashes nothing.
- **Uninstall** restores the stash exactly, or deletes the key when there was nothing to restore.
  A settings file agentgear created and then emptied by taking its own key back goes too; a file
  holding anything else of yours stays. A slot whose command matches neither the host's current
  declaration nor the last one agentgear wrote there is left untouched: someone else owns it now.
- **Ownership is the command string, current or last-written.** Edit the `padding` on the host's
  line and the next `self_heal` puts it back (that is drift); replace the command with something
  genuinely foreign and agentgear treats the slot as yours and stops touching it. Replacing it with
  a command agentgear itself put there does not hand ownership away: the marker records the command
  it last wrote, so a release that renames its own status-line subcommand still recognizes its own
  earlier value across the rename instead of stashing it over the user's real original.
- **A harness's own on/off switch is preserved, never overridden.** antigravity-cli's `enabled` is
  the only one today. If you had switched your status line off before installing, it stays off:
  agentgear will not flip a preference you set. `doctor` warns in that state instead of reporting a
  clean install, so a bar that renders nothing is never silent.
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
row). A user command that cannot start, prints nothing, runs past a 3-second timeout, or would
re-enter this same binary's own status-line render simply contributes nothing, so a broken command
of theirs never blanks the host's own bar and a stash naming an old, still-dispatchable version of
the host's own command stops one level deep instead of recursing. `user_original` returns the
stashed declaration directly if a host wants to render it itself; that reader is not guarded by the
re-entry check, since it never spawns anything.

Declare the command with [`${AGENTGEAR_CLIENT}`](Plugin-Tree#agentgear_client-per-harness-client-id)
(`mytool statusline --client ${AGENTGEAR_CLIENT}`) and each backend expands it to its own id, so
the host's subcommand knows which harness invoked it. Once two or more of your declared agents can
write a slot, leaving the token out is a [`doctor`](Doctor) warning: every harness would receive
the same literal command, so your subcommand reads one backend's stashed line from all of them and
either drops the user's own row or runs another harness's stashed command.

## See also

- [Capabilities](Capabilities) — the index.
- [Getting started](Getting-Started) — the `statusline_fn` derive attribute.
- [Doctor](Doctor) — the check that reports who owns the slot.
