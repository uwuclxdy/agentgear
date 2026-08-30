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
CMD_DIR="$QWEN_HOME/commands/ez-fixture-plugin"
AGENT="$QWEN_HOME/agents/ez-fixture-plugin-ez-helper.md"
SKILL_DIR="$QWEN_HOME/skills/ez-skill"
SKILL="$SKILL_DIR/SKILL.md"
mkdir -p "$QWEN_HOME"

# Seed a foreign mcp server, an unrelated key, and a user's own ui.statusLine — the
# retired-slot contract: setup/uninstall must leave the slot's value byte-identical,
# while the mcp + hook translation legitimately merges other keys beside it.
cat > "$SETTINGS" <<'JSON'
{ "theme": "dark", "ui": { "hideWindowTitle": true, "statusLine": { "type": "command", "command": "their-line", "padding": 2 } }, "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
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

# Retired-slot contract, parsed for real rather than grepped: the user's seeded
# ui.statusLine must still be exactly what it was after setup, beside the mcp/hook keys
# the translation legitimately merged.
node -e '
const fs = require("fs");
const [settingsPath] = process.argv.slice(1);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
const slot = settings.ui && settings.ui.statusLine;
const want = { type: "command", command: "their-line", padding: 2 };
if (JSON.stringify(slot) !== JSON.stringify(want)) {
  console.error("ui.statusLine changed across setup, want", JSON.stringify(want), "got", JSON.stringify(slot));
  process.exit(1);
}
if (settings.ui.hideWindowTitle !== true) {
  console.error("the seeded ui sibling was lost:", JSON.stringify(settings.ui));
  process.exit(1);
}
' "$SETTINGS" || { echo "FAIL: ui.statusLine after setup is wrong (see node error above)"; cat "$SETTINGS"; exit 1; }

host_fixture uninstall

if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$SETTINGS"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -d "$CMD_DIR" ];                then echo "FAIL: command dir survived uninstall"; exit 1; fi
if [ -f "$AGENT" ];                  then echo "FAIL: agent file survived uninstall"; exit 1; fi
if [ -d "$SKILL_DIR" ];              then echo "FAIL: skill dir survived uninstall"; exit 1; fi

# Same retired-slot contract across uninstall: the user's ui.statusLine survives
# byte-identical, its ui sibling intact.
node -e '
const fs = require("fs");
const [settingsPath] = process.argv.slice(1);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
const slot = settings.ui && settings.ui.statusLine;
const want = { type: "command", command: "their-line", padding: 2 };
if (JSON.stringify(slot) !== JSON.stringify(want)) {
  console.error("ui.statusLine changed across uninstall, want", JSON.stringify(want), "got", JSON.stringify(slot));
  process.exit(1);
}
if (!settings.ui || settings.ui.hideWindowTitle !== true) {
  console.error("the seeded ui sibling was lost:", JSON.stringify(settings.ui));
  process.exit(1);
}
if (!Object.hasOwn(settings, "theme")) {
  console.error("the seeded theme key was lost");
  process.exit(1);
}
' "$SETTINGS" || { echo "FAIL: settings.json state after uninstall is wrong (see node error above)"; cat "$SETTINGS"; exit 1; }

echo "qwen-code docker leg: OK"
TEST
chmod +x /usr/local/bin/qwen-code-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/qwen-code-leg.sh"]
