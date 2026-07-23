# Skills

A skill is a `skills/<name>/` directory with a `SKILL.md` (plus any assets) — a model-invocable
capability the agent loads on demand. agentgear writes each as a loose, tag-owned `<name>/SKILL.md`
in the target tool's skill root through one shared renderer.

**13 backends serve skills.** `claude` and `copilot-cli` carry them natively (the whole tree copies
verbatim). The other **11 — crush, cursor, devin, droid, goose, kilo, kimi, kiro, openclaw,
qwen-code, zed — render the `SkillDir` IR through the shared `skillsdir` renderer** as a tag-owned
`<name>/SKILL.md`.

## Where each skill lands

| harness | where written | notes |
|---|---|---|
| claude | `skills/<name>/` (native) | `SKILL.md` is the one thing CC hot-reloads |
| copilot-cli | native, verbatim | live-proven (`copilot skill list` sees a plugin skill) |
| crush | `~/.config/crush/skills/<name>/SKILL.md` (user) · `<project>/.crush/skills/<name>/SKILL.md` (project) | tag-owned |
| cursor | `~/.cursor/skills/<name>/SKILL.md` (both scopes) | tag-owned |
| devin | shared `~/.agents/skills/<name>/SKILL.md` | tag-owned; one of devin's native scan roots |
| droid | `<base>/skills/<name>/SKILL.md` (user + project) | tag-owned |
| goose | `~/.agents/plugins/<plugin>/skills/<name>/SKILL.md` | tag-owned, inside the plugin dir goose owns |
| kilo | `<base>/skills/<name>/SKILL.md` in every discovered config dir | tag-owned |
| kimi | `{$KIMI_CODE_HOME\|~/.kimi-code}/skills/<name>/SKILL.md` | tag-owned |
| kiro | `~/.kiro/skills/<name>/SKILL.md` | tag-owned, auto-inherited by every agent |
| openclaw | `~/.openclaw/skills/<name>/SKILL.md` | tag-owned |
| qwen-code | `{$QWEN_HOME\|~/.qwen}/skills/<name>/SKILL.md` (both scopes) | tag-owned |
| zed | `~/.agents/skills/` (global) · `<worktree>/.agents/skills/` (project, trust-gated) | tag-owned, 50 KB catalog budget |

## The shared `.agents/skills/` convention

A cross-harness convention (pi's docs name it `agentskills.io`): amp, cline, crush, cursor, devin,
goose, kimi, qwen-code, vscode-copilot, and zed each independently read `~/.agents/skills/` as a
discovery root. devin and zed are the two agentgear writes into; the rest read it without agentgear
writing there.

## Gotchas

- **Real but untranslated.** amp, antigravity, augment, cline, codex, gemini, omp, opencode, pi, and
  vscode-copilot each have a skill surface agentgear does not write yet (the tool reads a `SKILL.md`
  root, just not one agentgear targets). These are open upgrades, not harness limits.
- **A loose `skills/foo.md` is ignored.** The directory level is required — a skill needs its own
  `skills/<name>/` dir with a `SKILL.md` inside. See [Plugin tree](Plugin-Tree#what-is-parsed-from-where).
- **Tag-owned means safe to remove.** Each written `SKILL.md` carries an ownership tag, so `remove`
  and the retired-path sweep delete only agentgear's files; a user's same-named skill survives.

## See also

- [Capabilities](Capabilities) — the index.
- [Agent backends](Agent-Backends) — the `skillsdir` renderer and adding a backend.
- [Commands](Capability-Commands) · [Agents](Capability-Agents) — the sibling surfaces.
