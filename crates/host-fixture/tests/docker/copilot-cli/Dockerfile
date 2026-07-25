# syntax=docker/dockerfile:1
# copilot-cli docker leg: the real GitHub Copilot CLI ingesting our plugin natively.
# copilot >= 1.0.71 exposes a full `plugin` lifecycle (marketplace add / install /
# list / uninstall) that copies the CC tree VERBATIM, so agentgear orchestrates the
# CLI instead of translating config files. `plugin list` + `mcp list` are auth-free
# (verified 2026-07-18, docs/research/verify-copilot-cli.md), so both are
# authoritative asserts. Built + run by tests/docker/run.sh copilot-cli (context =
# repo root).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real copilot CLI (Node >=22) + the fixture binary; drive the native
# plugin lifecycle under a throwaway HOME/COPILOT_HOME.
FROM node:22-slim
# pin = the plugin-management floor, verified against a live install; bump it
# alongside docs/harness/copilot-cli.md.
RUN npm install -g @github/copilot@1.0.71
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/copilot-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/copilot-home
export COPILOT_HOME="$HOME/.copilot"
export XDG_DATA_HOME="$HOME/.local/share"
mkdir -p "$COPILOT_HOME" "$XDG_DATA_HOME"

# The statusLine slot lives outside copilot's plugin registry, in the very same
# settings.json real `copilot plugin marketplace add`/`plugin install` write
# `extraKnownMarketplaces`/`enabledPlugins` into (confirmed against a live container
# write, not assumed). `plugin list`/`mcp list` below read a SEPARATE file
# (config.json's `installedPlugins`) and stay green even with settings.json deleted or
# corrupted, so they prove nothing about our slot — what this leg DOES prove, uniquely,
# is that two independent writers (real copilot, and agentgear's own confedit)
# read-modify-write the one settings.json without clobbering each other: every
# hermetic test has exactly one writer, since `fake_copilot` persists its own registry
# to `fake-copilot-state.json` and never touches settings.json.
#
# `copilot plugin uninstall` preserves a foreign key intact (verified: a seeded
# `theirSetting` and `extraKnownMarketplaces` both survive it; copilot exposes no
# `marketplace remove`, matching this backend's own `remove()` doc) — that is what
# makes "statusLine absent after uninstall" mean WE removed it, not copilot eating it.
# Not permanent: the bundle's own settings allow-list is `model/remoteControl/
# enabledPlugins/extraKnownMarketplaces/permissions/strictKnownMarketplaces/telemetry`
# (grepped out of the installed package's app.js) — statusLine is not on it, so a
# future pin narrowing the writer to that list would make statusLine vanish on
# copilot's own write and the post-uninstall assert below would pass for the wrong
# reason. Recorded here so the next version bump can check for it.
#
# Passed as argv, not env, so neither `host_fixture` nor `copilot` inherits a variable
# this harness invented.
SETTINGS="$COPILOT_HOME/settings.json"
STATUSLINE_CMD="host_fixture statusline --client copilot-cli"

# install: agentgear materializes the CC tree, `copilot plugin marketplace add`s it,
# then `copilot plugin install ez-fixture-plugin@ez-fixture`.
out=$(host_fixture setup --agent copilot-cli)
if [ "$out" != "Installed" ]; then echo "FAIL: first setup was not Installed: $out"; exit 1; fi

# native assert #1: copilot's own registry lists our plugin (proves the marketplace
# add + install landed).
if ! copilot plugin list | grep -q 'ez-fixture-plugin@ez-fixture'; then
    echo "FAIL: copilot plugin list missing our plugin"; copilot plugin list; exit 1
fi

# native assert #2 (auth-free): copilot loads the plugin's mcp server verbatim. It
# must appear under `Plugin servers` — agentgear writes no user mcp-config.json now,
# so a listed server proves NATIVE plugin ingestion, not the old translate path.
mcp=$(copilot mcp list 2>&1) || { echo "FAIL: copilot mcp list errored"; echo "$mcp"; exit 1; }
if ! echo "$mcp" | grep -q 'Plugin servers'; then echo "FAIL: no 'Plugin servers' section (native ingestion failed)"; echo "$mcp"; exit 1; fi
if ! echo "$mcp" | grep -q 'ez-fixture'; then echo "FAIL: copilot mcp list missing plugin server"; echo "$mcp"; exit 1; fi

# FILE assert, not native — nothing auth-free makes copilot render its status line,
# so this proves only that the slot landed in the right shape at the right path, not
# that copilot displays it. Parse for real rather than grep: a value landing at the
# wrong nesting depth would still pass a substring check.
node -e '
const fs = require("fs");
const [settingsPath, wantCommand] = process.argv.slice(1);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
const slot = settings.statusLine;
if (!slot || typeof slot !== "object" || Array.isArray(slot)) {
  console.error("no statusLine object at the root");
  process.exit(1);
}
if (slot.type !== "command") {
  console.error("statusLine.type is not \"command\":", JSON.stringify(slot));
  process.exit(1);
}
if (slot.command !== wantCommand) {
  console.error("statusLine.command mismatch, want", wantCommand, "got", JSON.stringify(slot));
  process.exit(1);
}
// The host declares padding 0 and render() emits it, so it is as much the rendered
// shape as type/command -- and it is the field convergence compares whole-value
// (statuslinejson.rs), so a drift here reads as a foreign owner, not a clean install.
if (slot.padding !== 0) {
  console.error("statusLine.padding is not 0:", JSON.stringify(slot));
  process.exit(1);
}
// Coexistence: real copilot wrote these two keys into this same file before we
// wrote the slot. Our write is a whole-file rewrite (confedit::json_edit), so
// losing either here would mean it clobbered what copilot wrote instead of
// merging beside it -- the one thing no hermetic test can catch.
for (const key of ["extraKnownMarketplaces", "enabledPlugins"]) {
  if (!Object.hasOwn(settings, key)) {
    console.error(key, "written by copilot was lost when we wrote the slot");
    process.exit(1);
  }
}
' "$SETTINGS" "$STATUSLINE_CMD" || { echo "FAIL: settings.json state after setup is wrong (see node error above)"; cat "$SETTINGS"; exit 1; }

# a second setup must be a true NoOp (present + current version).
if [ "$(host_fixture setup --agent copilot-cli)" != "NoOp" ]; then echo "FAIL: second setup was not NoOp"; exit 1; fi

host_fixture uninstall

if copilot plugin list | grep -q 'ez-fixture-plugin@ez-fixture'; then echo "FAIL: our plugin survived uninstall"; exit 1; fi
if copilot mcp list 2>&1 | grep -q 'ez-fixture'; then echo "FAIL: plugin mcp server survived uninstall"; exit 1; fi

# FILE assert: nothing was stashed (the container never had a pre-existing status
# line), so `remove` deletes the key outright rather than restoring anything.
# extraKnownMarketplaces is asserted unconditionally: copilot has no `marketplace
# remove`, so it must survive OUR uninstall too, same as it survives copilot's own
# (see the header comment above) — this file must exist and hold that key.
node -e '
const fs = require("fs");
const [settingsPath] = process.argv.slice(1);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
if (!Object.hasOwn(settings, "extraKnownMarketplaces")) {
  console.error("extraKnownMarketplaces written by copilot was lost by our slot removal");
  process.exit(1);
}
if (Object.hasOwn(settings, "statusLine")) {
  console.error("statusLine key still present:", JSON.stringify(settings.statusLine));
  process.exit(1);
}
' "$SETTINGS" || { echo "FAIL: settings.json state after uninstall is wrong (see node error above)"; cat "$SETTINGS"; exit 1; }

echo "copilot-cli docker leg: OK"
TEST
chmod +x /usr/local/bin/copilot-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/copilot-leg.sh"]
