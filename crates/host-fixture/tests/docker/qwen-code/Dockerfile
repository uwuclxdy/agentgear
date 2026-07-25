# syntax=docker/dockerfile:1
# qwen-code docker leg: the real qwen-code CLI installed alongside our written
# config. Built + run by tests/docker/run.sh qwen-code (context = repo root).
# qwen-code exposes no headless `mcp list`/validate subcommand (only the interactive
# `/mcp` slash command), so a config-file parse is the source of truth; `qwen
# --version` is a best-effort proof the pinned CLI runs.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real qwen-code + the fixture binary; assert the lifecycle end-to-end.
# qwen-code requires Node.js >= 22.
FROM node:22-slim
# Pinned to a known-good tag (docker leg green on it); bump alongside
# docs/harness/qwen-code.md. qwen-code's release cadence is fast.
RUN npm install -g @qwen-code/qwen-code@0.19.10
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/qwen-code-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/qwen-home
export QWEN_HOME="$HOME/.qwen"
SETTINGS="$QWEN_HOME/settings.json"
# Passed as argv, not env, so `host_fixture` never inherits a variable this harness
# invented. `${AGENTGEAR_CLIENT}` in the host declaration expands to the backend id.
STATUSLINE_CMD="host_fixture statusline --client qwen-code"
CMD_DIR="$QWEN_HOME/commands/ez-fixture-plugin"
AGENT="$QWEN_HOME/agents/ez-fixture-plugin-ez-helper.md"
SKILL_DIR="$QWEN_HOME/skills/ez-skill"
SKILL="$SKILL_DIR/SKILL.md"
mkdir -p "$QWEN_HOME"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
# `ui.hideWindowTitle` seeds the CONTAINER our slot writes into, not just the root:
# qwen is the only nested slot in the fleet, so it is the only leg where the container
# walk itself can go wrong, and a root-level key alone cannot see that.
cat > "$SETTINGS" <<'JSON'
{ "theme": "dark", "ui": { "hideWindowTitle": true }, "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent qwen-code

if ! grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our mcp server not registered"; cat "$SETTINGS"; exit 1; fi
if ! grep -q 'SessionStart' "$SETTINGS"; then echo "FAIL: our hooks not written"; exit 1; fi
if ! grep -q 'theirs'     "$SETTINGS"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'    "$SETTINGS"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$CMD_DIR/hello.md" ];       then echo "FAIL: copied command markdown missing"; exit 1; fi
if [ ! -f "$AGENT" ];                  then echo "FAIL: rendered agent markdown missing"; exit 1; fi
if [ ! -f "$SKILL" ];                  then echo "FAIL: translated skill SKILL.md missing"; exit 1; fi
if ! grep -q 'x-agentgear' "$SKILL";   then echo "FAIL: skill ownership tag missing"; exit 1; fi

# Native sanity: the pinned CLI runs. No headless mcp-list exists, so the config
# parse above is authoritative; this only proves the package installed + launches.
if qwen --version >/dev/null 2>&1; then
  echo "qwen --version ok"
else
  echo "note: qwen --version unavailable; relied on config-file assert"
fi

# FILE assert, not native — nothing auth-free makes qwen render its status line, so
# this proves only that the slot landed in the right shape at the right path, not that
# qwen displays it. Unlike copilot-cli there is no two-writer coexistence to prove
# here: the pinned CLI writes this file for neither `qwen --version` nor anything else
# auth-free (checked by md5 across the call in this image), so the seeded `theme` and
# `theirs` asserted above are the only foreign keys in play. Ordered AFTER the version
# call anyway, so a future qwen that normalizes settings.json on startup reds here.
#
# Parse for real rather than grep: qwen nests its slot one level down, under `ui`, and
# a value at the wrong depth passes any substring check. The grep above is blind to
# this key for a second reason — it matches `ez-fixture`, while our command string
# says `host_fixture`.
node -e '
const fs = require("fs");
const [settingsPath, wantCommand] = process.argv.slice(1);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
if (Object.hasOwn(settings, "statusLine")) {
  console.error("statusLine written at the ROOT; qwen reads ui.statusLine and would ignore it");
  process.exit(1);
}
const ui = settings.ui;
if (!ui || typeof ui !== "object" || Array.isArray(ui)) {
  console.error("no ui object to hold the slot");
  process.exit(1);
}
const slot = ui.statusLine;
if (!slot || typeof slot !== "object" || Array.isArray(slot)) {
  console.error("no statusLine object under ui");
  process.exit(1);
}
// type is the load-bearing discriminator, not decoration: qwen gates on
// type === "command" and re-reads anything failing that as a built-in PRESET, so a
// wrong type means the declared command silently never runs.
if (slot.type !== "command") {
  console.error("ui.statusLine.type is not \"command\":", JSON.stringify(slot));
  process.exit(1);
}
if (slot.command !== wantCommand) {
  console.error("ui.statusLine.command mismatch, want", wantCommand, "got", JSON.stringify(slot));
  process.exit(1);
}
// The host declares padding 0 and render() emits it. qwen itself drops the key on
// read, so it renders nothing here -- but it is still the field convergence compares
// whole-value (statuslinejson.rs), so a drift reads as a foreign owner.
if (slot.padding !== 0) {
  console.error("ui.statusLine.padding is not 0:", JSON.stringify(slot));
  process.exit(1);
}
// Coexistence INSIDE the container, which only this backend has: the write walks into
// an existing ui rather than creating it, so a regression that replaced the object
// wholesale would still leave a correct slot at a correct depth and pass every check
// above.
if (ui.hideWindowTitle !== true) {
  console.error("the seeded ui sibling was lost when the slot was written:", JSON.stringify(ui));
  process.exit(1);
}
' "$SETTINGS" "$STATUSLINE_CMD" || { echo "FAIL: ui.statusLine after setup is wrong (see node error above)"; cat "$SETTINGS"; exit 1; }

host_fixture uninstall

if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$SETTINGS"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -d "$CMD_DIR" ];                then echo "FAIL: command dir survived uninstall"; exit 1; fi
if [ -f "$AGENT" ];                  then echo "FAIL: agent file survived uninstall"; exit 1; fi
if [ -d "$SKILL_DIR" ];              then echo "FAIL: skill dir survived uninstall"; exit 1; fi

# FILE assert: nothing was stashed (the container never had a pre-existing status
# line), so `remove` deletes the slot key outright rather than restoring anything. The
# `ui` container SURVIVING is asserted, its emptiness is not: remove navigates without
# creating and deletes only the leaf, so a container left holding just the user's own
# keys is the contract, and one deleted along with the leaf is a regression.
node -e '
const fs = require("fs");
const [settingsPath] = process.argv.slice(1);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
if (Object.hasOwn(settings, "statusLine")) {
  console.error("a statusLine key appeared at the root:", JSON.stringify(settings.statusLine));
  process.exit(1);
}
if (settings.ui && Object.hasOwn(settings.ui, "statusLine")) {
  console.error("ui.statusLine still present:", JSON.stringify(settings.ui.statusLine));
  process.exit(1);
}
if (!settings.ui || settings.ui.hideWindowTitle !== true) {
  console.error("the seeded ui sibling was lost by our slot removal:", JSON.stringify(settings.ui));
  process.exit(1);
}
if (!Object.hasOwn(settings, "theme")) {
  console.error("the seeded theme key was lost by our slot removal");
  process.exit(1);
}
' "$SETTINGS" || { echo "FAIL: settings.json state after uninstall is wrong (see node error above)"; cat "$SETTINGS"; exit 1; }

echo "qwen-code docker leg: OK"
TEST
chmod +x /usr/local/bin/qwen-code-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/qwen-code-leg.sh"]
