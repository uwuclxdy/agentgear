# Scopes & gates

Every install targets a scope: `Scope::User` (the home-level config) or `Scope::Project { path }`
(that directory's). A backend with no surface at the requested scope is skipped silently — a
project-scope install lands only on backends that serve project scope. `self_heal` runs at user scope
only (project scope has no stable session context to key on in v1).

Beyond the scope itself, a write may not take effect until a **trust gate** clears: a folder-trust
prompt, a content-hash confirmation, an approval command. Those gates are the second column of the
story.

## Scope support & gates

| harness | user | project | activation gates / caveats |
|---|---|---|---|
| claude | ✅ | ✅ | `Scope::Local` not exposed. self_heal is user-scope-only. entry lookup is scope-blind |
| amp | ✅ | ❌ | project resolver exists but is never reached: `.amp/` needs `amp mcp approve <name>`, scan is exact-CWD |
| antigravity | ✅ | ❌ | user-only |
| antigravity-cli | ✅ | ✅ | project scope needs folder trust |
| augment | ✅ | ❌ | the tool serves project + local; backend is user-only by design |
| cline | ✅ | ✅ | mcp is user/global-only; project config activates on cwd, no trust gate found |
| codex | ✅ | ✅ | project trust-gated (`trust_level="trusted"` in the user `config.toml`); once trusted, a session loads the project's hooks and mcp servers, though the `codex mcp` management CLI still only reads the user config |
| copilot-cli | ✅ | ❌ | native install only, no `--scope`. a native install registers as plugin-provided, outranking a same-named workspace server |
| crush | ✅ | ✅ | presence-only. a same-key entry in `$XDG_DATA_HOME/crush/crush.json` silently beats ours |
| cursor | ✅ | ✅ | no gate. project wins on a same-key mcp collision |
| devin | ✅ | ✅ | additive merge; project/local overrides user on a same name |
| droid | ✅ | ✅ | a 3rd tier sits between them (any ancestor's `.factory/mcp.json`), outside our write surface |
| gemini | ✅ | ✅ | mcp + commands suppressed blanket in an untrusted folder; hooks: project blocked, user unconditional |
| goose | ✅ | ❌ | no project extensions file |
| jetbrains-copilot | ✅ | ❌ | `<project>/.github/mcp.json` is real, unwritten |
| kilo | ✅ | ✅ | presence-only |
| kimi | ✅ | ❌ (declared) | `<cwd>/.kimi-code` is a real native mcp scope; undeclared because hooks have no project surface |
| kiro | ✅ | ✅ | no trust gate documented |
| omp | ✅ | ✅ | presence-only. `OMP_PROFILE`/`PI_PROFILE` relocate user scope, untracked |
| openclaw | ✅ | ❌ | one user-global file regardless of cwd |
| opencode | ✅ | ✅ | presence-only, no trust/enable gate found |
| pi | ✅ | ❌ | detect-only backend, nothing written at either scope |
| qwen-code | ✅ | ✅ | project hooks need folder trust; project mcp/commands/agents apply unconditionally |
| vscode-copilot | ❌ | ✅ | user scope rejected on purpose: a non-default profile relocates `User/mcp.json`, and the active profile is VS Code-internal state |
| zed | ✅ | ✅ | project write is unconditional; `trust_all_worktrees` gates only whether zed auto-starts project mcp servers |

`vscode-copilot` is the one project-only backend. Nine backends are user-scope only; the rest serve
both.

## Trust gates worth knowing

- **codex** hooks are written but stay **inert** until the user trusts them in codex's `/hooks` TUI (a
  content-hash gate), or passes `--dangerously-bypass-hook-trust`.
- **Folder trust** gates the project scope on antigravity-cli, gemini (project), and qwen-code
  (project hooks).
- **amp** needs `amp mcp approve <name>` for a project server to activate.
- **gemini** re-consents on a hook's name+command fingerprint even at user scope.

## See also

- [Capabilities](Capabilities) — the index.
- [Hooks](Capability-Hooks) — the per-harness hook trust gates in full.
- [Getting started](Getting-Started) — passing a scope to `install`.
