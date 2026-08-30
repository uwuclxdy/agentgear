# Status line (manual wiring)

A status line is the bar a coding agent renders under (or over) its prompt, filled by running a
command and printing its stdout. agentgear used to deliver one automatically — a host declared a
`StatusLineDecl`, and five backends wrote it into their harness's single status-line slot, stashing
and restoring whatever the user had. That automatic wiring is retired (2026-08-30): agentgear
writes no harness's status-line slot anymore, and a status line you already had is left
byte-identical by every lifecycle operation.

What remains is the render half. A host ships a `statusline` print subcommand — it reads the
session JSON on stdin and prints the bar — and you wire that subcommand into your harness config
yourself, if you want it. The public helpers the subcommand is built from:

```rust
// in the host's own `statusline` subcommand
let mut session = String::new();
std::io::stdin().read_to_string(&mut session)?;
let line = agentgear::statusline::compose(&MyHost::descriptor(), client, &session, &my_rows)?;
println!("{line}");
```

`compose` returns the host's own rows first, then runs the user's original command with the same
session JSON on its stdin and appends its rows (the surface is line-oriented: one line is one
row). `user_original` reads the "original" off the host's own stamp marker — only installs made by
pre-retirement binaries stashed one, so on a fresh install it returns nothing and the host's own
rows render alone. A user command that cannot start, prints nothing, or runs past a 3-second
timeout simply contributes nothing, so a broken command of theirs never blanks the host's own bar;
a render already running inside another render is refused at the spawn boundary, so a stash naming
the host's own command stops one level deep instead of recursing.

## Slot facts, for wiring one by hand

Five harnesses carry a single status-line slot in their own settings file; the other twenty have
none (where their settings look like a slot, the value is a curated list of built-in widget ids,
not a command). These rows are the harnesses' own surfaces, kept for anyone writing a line by hand:

| harness | file | key | value shape |
|---|---|---|---|
| claude | `<config>/settings.json` (user, `CLAUDE_CONFIG_DIR`) or `<project>/.claude/settings.json` | `statusLine` | `{"type":"command","command":…}` plus optional `padding` |
| copilot-cli | `settings.json` under `$COPILOT_HOME` (else `~/.copilot`), user scope only | `statusLine` | same shape as Claude Code's; the renderer reads `command` and `padding` |
| qwen-code | `settings.json` under `$QWEN_HOME` (else `~/.qwen`), both scopes | `ui.statusLine` | `type: "command"` is required there: anything else is silently read as a built-in preset and the command never runs |
| antigravity-cli | `~/.gemini/antigravity-cli/settings.json`, user scope only | `statusLine` | carries that harness's own `enabled` on/off toggle |
| droid | `~/.factory/settings.json` (user) or `<project>/.factory/settings.json` | `statusLine` (root-level on disk) | accepts an optional `maxRows` (int 1..3); a two-row line needs it raised past the default of 1 |

## See also

- [Capabilities](Capabilities) — the index.
- [Getting started](Getting-Started) — the derive attributes.
