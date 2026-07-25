# Types and errors

The programmatic surface: what a host reads when it wants structured results instead of a
printed `Display` summary. The result enums a host matches on (`Outcome`, `AgentStatus`,
`SkipReason`, `CheckStatus`, `Error`) are `#[non_exhaustive]`, so give them a wildcard arm.
The data types a backend builds or reads by field (`Capabilities`, `AgentResult`, the
components IR) are not, so you can construct and exhaustively match those.

## Outcome

What a lifecycle op actually did.

| variant | meaning | `Display` |
|---|---|---|
| `NoOp` | already converged, nothing changed | `no changes needed` |
| `Installed` | a fresh install | `installed` |
| `Updated { from: Option<String>, to: String }` | version bump; `from` is `None` when the prior version could not be read | `updated (X -> Y)` or `updated (to Y)` |
| `Repaired` | a broken or partial state was reconciled back to healthy | `repaired` |
| `Adopted` | self_heal found a healthy install with no marker and wrote one | `adopted existing install` |
| `Removed` | uninstalled | `removed` |
| `Cleared` | self_heal found a cleanly-uninstalled plugin and cleared the stale marker | `cleared stale marker` |

## AgentReport, AgentResult, AgentStatus, SkipReason

Every `*_report` lifecycle method (`install_report`, `install_into_report`, `update_report`,
`uninstall_report`, `self_heal_report`) returns `Result<AgentReport>`. The plain methods
(`install`, `update`, ...) collapse the same report to one `Outcome` and surface any per-agent
failure as `Err(Error::Backend)`.

```rust
pub struct AgentReport {
    pub results: Vec<AgentResult>,
}

pub struct AgentResult {
    pub agent: &'static str,   // the backend id, "claude", "codex", ...
    pub status: AgentStatus,
}

pub enum AgentStatus {
    Converged(Outcome),   // ran; this is its own outcome, not the merged one
    Skipped(SkipReason),  // never got to write anything
    Failed(String),       // failed; rendered for the user, fan-out continued past it
}

pub enum SkipReason {
    NotDetected,        // detect() returned false
    ScopeUnsupported,    // no config surface at the requested scope
    SourceUnsupported,   // a GitHub source has no local tree for a config-merge backend
}
```

`results` holds one entry per agent `reconcile`/`remove` was asked to run; an agent excluded by an
explicit `install_into` filter gets no entry at all, distinct from being skipped. Read the vector
directly to build a per-agent UI, or use the two convenience methods:

- `AgentReport::merged() -> Outcome`: the first real change across the agents (a change outranks a
  no-op), `Outcome::NoOp` when nothing changed. This is what the plain lifecycle methods return.
- `AgentReport::is_healthy() -> bool`: true when no agent's status is `Failed`. Skips don't count
  as unhealthy.

`AgentReport` and `AgentStatus` both have a `Display` impl (one `agent: status` line per result,
using `Outcome`'s or `SkipReason`'s own wording), a ready `setup` summary when a host just wants
to print it.

## Capabilities

What one `AgentBackend` can host. A backend reports these as fixed, compile-time-known facts about
the tool it targets, not runtime detection.

| field | type | meaning |
|---|---|---|
| `plugins` | `bool` | native plugin install (copies the whole tree); implies every other surface except `instructions` |
| `mcp` | `bool` | can host MCP servers |
| `hooks` | `bool` | can host hook bindings |
| `commands` | `bool` | can host slash-command definitions |
| `agents` | `bool` | can host agent (subagent) definitions |
| `skills` | `bool` | can host skill directories |
| `instructions` | `bool` | can receive host-authored always-loaded guidance through its own native context channel (a dedicated file plus any registration); `false` for a plugin-native backend, which delivers guidance through its own channel instead |
| `statusline` | `bool` | manages the tool's status-line slot from `PluginHost::statusline`. Not implied by `plugins`: the slot lives in the tool's own settings file, outside any plugin tree, so a plugin-native backend still writes it (`claude` does; the other three `true` today are `qwen-code`, `antigravity-cli`, `droid`) |
| `scopes` | `&'static [&'static str]` | which of `"user"`/`"project"` this backend can target |

`setup` reads this to report a partial fit ("this agent hosts MCP servers, not hooks") instead of
dropping features silently; it is the runtime truth behind the per-tool coverage table on
[Harness comparison](Harness-Comparison).

## DoctorReport, DoctorCheck, CheckStatus

`doctor()`'s `Display` is covered on the [Doctor](Doctor) page; this is the shape for a host that
wants to walk the checks itself instead of printing them.

```rust
pub struct DoctorReport { /* checks: Vec<DoctorCheck>, private */ }

pub struct DoctorCheck {
    pub name: &'static str,
    pub status: CheckStatus,
}

pub enum CheckStatus {
    Ok(String),
    Warn(String),
    Fail { problem: String, fix: String },
}
```

`checks` is private so a report is always built whole; read it back with `.checks() -> &[DoctorCheck]`.
`.is_healthy() -> bool` is true when no check is `Fail` (a `Warn` is tolerated).

Building a report is public too, for an external `AgentBackend`'s own `report()`:
`DoctorReport::from_checks(checks: Vec<DoctorCheck>)` wraps a check list as-is;
`DoctorReport::from_error(err: Error)` collapses an upfront failure (say, an unreadable config)
into a single `Fail` check named `"doctor"` with `fix: "see the error above"`.

## The components IR

`Plugin::components(&source) -> Result<PluginComponents>` parses a plugin tree once into a
harness-agnostic shape every non-CC backend renders from, instead of each one walking the tree by
hand. Errors on `Source::GitHub`: a config-merge backend has no local tree to parse from a github
ref.

```rust
pub struct PluginComponents {
    pub mcp_servers: Vec<McpServer>,
    pub hooks: Vec<HookBinding>,
    pub commands: Vec<MarkdownDoc>,
    pub agents: Vec<MarkdownDoc>,
    pub skills: Vec<SkillDir>,
}
```

| type | fields | notes |
|---|---|---|
| `McpServer` | `name: String`, `kind: McpKind`, `command: String`, `args: Vec<String>`, `env: BTreeMap<String, String>` | `.is_portable()` is false when `command`/`args` carry `${CLAUDE_PLUGIN_ROOT}` |
| `McpKind` | `Stdio`, `Http { url: String }`, `Sse { url: String }` | |
| `HookBinding` | `event: String`, `matcher: Option<String>`, `command: String` | `.is_portable()` mirrors `McpServer`'s, checked against `command` |
| `MarkdownDoc` | `name: String` (file stem), `rel: String` (the file's path in the tree), `frontmatter: BTreeMap<String, Value>`, `body: String`, `raw: Vec<u8>` (verbatim file bytes) | backs both `commands` and `agents` |
| `SkillDir` | `name: String`, `files: Vec<(String, Vec<u8>)>` (each file's path under the skill dir, and its bytes) | |

What gets parsed from where, and the frontmatter parser's limits, are on
[Plugin tree](Plugin-Tree#what-is-parsed-from-where).

## Error

Every fallible call in the crate returns `Result<T, Error>` (`type Result<T> = std::result::Result<T, Error>`).

| variant | meaning |
|---|---|
| `ClaudeNotFound` | `claude` is not on PATH |
| `ClaudeTooOld { found, floor }` | `claude` is below the required version floor |
| `CopilotNotFound` | `copilot` is not on PATH |
| `CopilotTooOld { found }` | `copilot` is below 1.0.71, the plugin-management floor |
| `Cli { bin, args, code, stderr }` | a CLI invocation exited nonzero |
| `Json { what, source }` | a JSON parse failed |
| `Io { context, source }` | a filesystem operation failed |
| `Tree(String)` | the embedded or path plugin tree is malformed (an authoring bug in the host, e.g. missing `plugin.json` or `author`) |
| `Config { path, detail }` | a harness config file exists but does not parse; the read-modify-write refuses to clobber it rather than risk destroying the user's config |
| `Backend { agent, detail }` | one agent's failure, rendered as a string |
| `Verify(String)` | a mutating CLI call exited 0 but `list --json` does not reflect the expected end state |
| `Lock { path, source }` | failed to acquire the shared `flock` |

`Backend` is the one variant an out-of-crate `AgentBackend` should return for its own failures,
since every other variant carries in-crate semantics (CLI orchestration, tree parsing, config
merging) that don't fit a tool this crate never shipped a backend for. It does double duty
internally too: the plain lifecycle methods collapse an `AgentReport` to one `Result<Outcome>`, so any
agent's `Failed(String)`, built-in or external, becomes `Error::Backend` there too. The `*_report`
methods skip that collapse entirely; a per-agent failure stays inside `AgentStatus::Failed` and the
call still returns `Ok`.
