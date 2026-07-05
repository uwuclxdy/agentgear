# Doctor

`doctor()` returns a structured `DoctorReport`. Each check carries a rendered fix-hint, so a failing environment tells the user what to do. `Display` prints a readable summary; `is_healthy()` is true when no check failed (warnings are tolerated).

## The checks

| # | check | passes when | on failure |
|---|---|---|---|
| 1 | host binary on PATH | the running executable's name resolves via PATH | install the binary into a PATH directory, so the plugin's hooks can invoke it |
| 2 | claude version | `claude` is on PATH and at or above the floor `2.1.196` | upgrade Claude Code |
| 3 | plugin registered | `plugin list --json` parses and lists `name@marketplace` | run the host's `setup` |
| 4 | manifest validates | `claude plugin validate <current> --strict` is clean | fix the reported manifest issue, then `update` |
| 5 | current tree matches embedded | the materialized tree hashes equal to the embedded tree | re-run `update` to re-materialize a stale or corrupt pointer |
| 6 | hook commands on PATH | every bare command the plugin's hooks call resolves | install the missing binaries |

Checks 5 and 6 need no `claude`, so they run even when Claude Code is absent.

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

A common real breakage is check 6: the plugin's hooks call the host binary by name while the binary is not on the user's PATH. Check 5 catches a `current` pointer that went stale or corrupt in a way the version number cannot, since a matching version can still front a broken tree.
