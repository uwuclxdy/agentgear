# Doctor

`doctor()` returns a structured `DoctorReport`. Each check carries a rendered fix-hint, so a failing environment tells the user what to do. `Display` prints a readable summary; `is_healthy()` is true when no check failed (warnings are tolerated).

The report fans out over the host's configured agents. It opens with one shared check, then appends each agent's own checks. An agent the host declares but that is not installed on this machine contributes a single `not installed; skipped` line rather than a failure, the same way `install` and `self_heal` skip it.

## Shared check

| check | passes when | on failure |
|---|---|---|
| host binary on PATH | the running executable's name resolves via PATH | install the binary into a PATH directory, so the plugin's hooks can invoke it |

## Claude Code checks

The `claude` backend adds five. A claude-only host sees exactly these plus the shared check above:

| # | check | passes when | on failure |
|---|---|---|---|
| 1 | claude version | `claude` is on PATH and at or above the floor `2.1.196` | upgrade Claude Code |
| 2 | plugin registered | `plugin list --json` parses and lists `name@marketplace` | run the host's `setup` |
| 3 | manifest validates | `claude plugin validate <current> --strict` is clean | fix the reported manifest issue, then `update` |
| 4 | current tree matches embedded | the materialized tree hashes equal the embedded tree | re-run `update` to re-materialize a stale or corrupt pointer |
| 5 | hook commands on PATH | every bare command the plugin's hooks call resolves | install the missing binaries |

Checks 4 and 5 need no `claude`, so they run even when Claude Code is absent.

On a zero-embed host (`embed = false`, github source) check 4 reports `github source; not applicable`, but check 5 still reads the baked tree and warns `could not read the embedded tree` on every run. That warning is the expected steady state for such a host today, not a break.

## Config-merge agent checks

A detected config-merge backend contributes its own slice:

| check | passes when |
|---|---|
| `<id>` detected | the tool's CLI or config dir is present |
| config file | the tool's config file parses, or does not exist yet |
| mcp server registered | the plugin's server sits under the tool's mcp key in that config |
| mcp command on PATH | the server's bare command resolves on PATH |
| translated files present | the commands/agents/hooks agentgear wrote for the tool are on disk |

codex reports its translated hooks as a warning: they sit inert in its config until a human trusts them through codex's `/hooks` TUI. kimi has no trust gate, so its hooks check reports ok.

## Reading the output

```console
$ mytool doctor
[ ok ] host binary on PATH: `mytool` resolves on PATH
[ ok ] claude version: 2.1.201 (Claude Code)
[ ok ] plugin registered: mytool@mytool v0.4.0 (enabled)
[ ok ] manifest validates: ~/.local/share/mytool/current --strict clean
[ ok ] current tree matches embedded: hashes match
[fail] hook commands on PATH: hook command(s) not on PATH: mytool
       fix: install the missing binaries into a PATH directory
```

A multi-agent host appends each detected agent's checks below these, or a single `[ ok ] codex: not installed on this host; skipped` for one that is absent.

A common real breakage is check 5 (the last row): the plugin's hooks call the host binary by name, but it is not on the user's PATH. Check 4 catches a `current` pointer that went stale or corrupt in a way the version number cannot, since a matching version can still front a broken tree.
