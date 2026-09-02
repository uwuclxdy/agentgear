# Adopting an existing plugin

For a tool that **already ships a Claude Code plugin** and is adding agentgear to take the lifecycle over. Starting from nothing instead? [Getting started](Getting-Started) is the whole path and none of this applies.

The docs tell you how to adopt the crate. They do not, on their own, tell you what adopting does to the installs your users already have, and the re-point machinery reads as if it covers that. It does not cover it alone. Three things have to hold, each one harmless-looking to break, and one real consumer paid for all three at once.

## 1. Keep your committed root manifest

[Getting started](Getting-Started#2-lay-out-the-plugin-tree) says the crate generates `marketplace.json` so you do not ship one. That is true of the tree agentgear materializes. It is not true of the `.claude-plugin/marketplace.json` committed at your repo root, which is what a **GitHub-sourced registration reads**, and every registration made before you adopted is one of those.

Delete it and the next `claude plugin marketplace update` re-pulls your repo, finds no manifest, and the marketplace stops loading. Nobody has to type that command: Claude Code's `/plugin` UI drives it, and so do third-party updaters. What the user sees is nothing at all, because the failure lands on the plugin entry as `errors: [...]`, with `installPath` still resolving, and **a plugin whose marketplace fails to load serves 0 hooks**.

Your `SessionStart` self-heal is one of those hooks. The repair path goes down with the thing it repairs.

## 2. The in-plugin hook is forward-only, so it converges one release late

`self_heal` ships inside the plugin tree. The `SessionStart` hook that fires it therefore exists only for users whose installed tree came from a release that **already carried it**, so the release you adopt on migrates nobody through that path. It reaches the users of the release after it.

Same forward-only property as the restart-pending flag, and it bites harder here. A restart notice one release late costs a stale session. A heal one release late means your adoption release converges nothing, and you find out only when the reports do not stop.

Condition 3 is what removes the delay: a trigger in your own binary runs at whatever version the user's **binary** is, never at whatever version their plugin tree is. Ship both and the hook becomes the redundant path rather than the only one.

## 3. Ship at least one heal trigger that runs outside the plugin

A plugin that fails to load cannot repair itself, which is exactly condition 1's failure mode. Every trigger inside `hooks/hooks.json` is unreachable in the state you most need a trigger.

Wire `self_heal()` somewhere your binary reaches on its own: a daemon tick, your MCP server's startup, your launcher's pre-flight. Throttle it and run it detached so it never sits in front of a user-visible path. One out-of-plugin trigger is the difference between a migration that converges and one that waits for a subcommand your users have no reason to run.

## What converges, once all three hold

A pre-adoption registration reaches the local materialized marketplace on the first heal that runs. Its version relative to your binary does not change what happens: a GitHub-sourced entry under a binary that materializes its own tree is structurally divergent whatever its version, so every relation takes one sequence. `marketplace add` over the same name re-points it, then `plugin uninstall` + `plugin install` re-hands the tree, reported as `Repaired`. The reinstall is not optional: Claude Code keys its plugin cache on the version, so a re-pointed marketplace on its own leaves the user serving the old tree.

| the user's registration | before 0.1.3 | 0.1.3 and later |
|---|---|---|
| older than your binary | converges | converges |
| the same version | converges | converges |
| **newer** than your binary | never converges | converges |

The last row needs agentgear **0.1.3 or newer**. Below that, a strictly-newer registration returned a no-op before its divergence was ever read, so a user one release behind stayed on GitHub until they updated the binary, running the repo's current `hooks.json` against an older binary the whole time. What a newer version still buys is protection for a registration another build of your tool owns, which is the coexisting-binaries case and is untouched here ([How it works](How-It-Works#self-heal-state-table)).

## When the committed manifest can finally go

Not on adoption, and not on the release after it. The manifest is what keeps a **not-yet-migrated** registration loading, so it can go only once none are left.

One half of that is checkable and one is not. The checkable half is the anchor: the first of your releases that shipped both the `SessionStart` hook and an out-of-plugin trigger. Nobody below the anchor can migrate at all, so the anchor is the floor and you can name it. The other half, how many users are still below it, is not observable from here. Retiring the manifest is you deciding to strand them.

It costs one committed file, and retiring it early is silent for the user and invisible to you.
