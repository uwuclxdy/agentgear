# syntax=docker/dockerfile:1
# qwen-code docker leg: the real qwen-code CLI installed alongside our written
# config. Built + run by tests/docker/run.sh qwen-code (context = repo root).
# `qwen mcp` ships a headless add/remove/list, so the leg runs qwen's OWN native
# writer (`mcp add`) against the settings.json agentgear writes, then asserts our
# entries, the user's seeded keys and qwen's own entry all survive it — and that
# uninstall still removes only ours.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real qwen-code + the fixture binary; assert the lifecycle end-to-end.
# qwen-code requires Node.js >= 22.
FROM node:22-slim
# Pinned to a known-good tag (docker leg green on it). qwen-code's release cadence is
# fast.
RUN npm install -g @qwen-code/qwen-code@0.26.1
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/qwen-settings-check.js <<'JS'
// Parsed check of ~/.qwen/settings.json for one lifecycle phase. Prints one
// `key` line per missing/wrong key and exits nonzero; empty output = all present.
// The writer phase takes a third arg: the pre-writer copy, so our entries and
// hooks are asserted by raw identity (JSON.stringify) against it, not by field.
const fs = require("fs");
const [phase, settingsPath, prePath] = process.argv.slice(2);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
const pre = prePath ? JSON.parse(fs.readFileSync(prePath, "utf8")) : null;
const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const server = (name) => (settings.mcpServers || {})[name];
let bad = 0;
const fail = (key) => { console.error("  " + key); bad = 1; };

// Seeded user keys: must survive every phase at their seeded paths.
if (!settings.ui || settings.ui.theme !== "dark") fail("ui.theme");
if (!settings.ui || settings.ui.hideWindowTitle !== true) fail("ui.hideWindowTitle");
if (!settings.ui || !eq(settings.ui.statusLine, { type: "command", command: "their-line", padding: 2 })) fail("ui.statusLine");
if (settings.theirRootKey !== "keep") fail("theirRootKey");
if (!server("theirs") || server("theirs").command !== "their-server" || !eq(server("theirs").args, [])) fail("mcpServers.theirs");
if (!hookPresent("SessionStart", "their-hook")) fail("hooks.SessionStart (the user's their-hook missing)");

function hookPresent(event, command) {
  const list = settings.hooks && settings.hooks[event];
  if (!Array.isArray(list)) return false;
  return list.some((g) => Array.isArray(g.hooks) && g.hooks.some((h) => h.type === "command" && h.command === command));
}

function oursPresent() {
  if (!server("ez-fixture") || server("ez-fixture").command !== "host_fixture" || !eq(server("ez-fixture").args, ["mcp"])) fail("mcpServers.ez-fixture");
  if (!server("ez-fixture-http") || server("ez-fixture-http").httpUrl !== "http://127.0.0.1:39621/mcp") fail("mcpServers.ez-fixture-http");
  if (!server("ez-fixture-sse") || server("ez-fixture-sse").url !== "http://127.0.0.1:39622/sse") fail("mcpServers.ez-fixture-sse");
  if (!hookPresent("SessionStart", "host_fixture self-heal")) fail("hooks.SessionStart (our self-heal hook missing)");
  if (!hookPresent("UserPromptSubmit", "host_fixture check-restart")) fail("hooks.UserPromptSubmit (our check-restart hook missing)");
}

function writerPresent() {
  if (!server("qwen-native-server") || server("qwen-native-server").command !== "echo" || !eq(server("qwen-native-server").args, ["hello"])) fail("mcpServers.qwen-native-server");
}

// Our mcp entries and hooks must survive the native writer byte-for-byte against
// the pre-writer copy. qwen's writer preserves key order (measured 0.26.1), so
// raw JSON.stringify equality is the identity check.
function writerIdentity() {
  for (const k of ["ez-fixture", "ez-fixture-http", "ez-fixture-sse"])
    if (!eq(server(k), (pre.mcpServers || {})[k])) fail("mcpServers." + k + " (changed by the native writer)");
  for (const e of ["SessionStart", "UserPromptSubmit"])
    if (!eq((settings.hooks || {})[e], (pre.hooks || {})[e])) fail("hooks." + e + " (changed by the native writer)");
}

function oursAbsent() {
  if (server("ez-fixture") !== undefined) fail("mcpServers.ez-fixture (uninstall left it)");
  if (server("ez-fixture-http") !== undefined) fail("mcpServers.ez-fixture-http (uninstall left it)");
  if (server("ez-fixture-sse") !== undefined) fail("mcpServers.ez-fixture-sse (uninstall left it)");
  // our self-heal hook is gone; only the user's seeded foreign SessionStart hook remains.
  if (!settings.hooks || !eq(settings.hooks.SessionStart, [{ hooks: [{ type: "command", command: "their-hook" }] }])) fail("hooks.SessionStart (uninstall removed the user's hook or left ours)");
  if (settings.hooks && settings.hooks.UserPromptSubmit !== undefined) fail("hooks.UserPromptSubmit (uninstall left it)");
}

if (phase === "setup") {
  oursPresent();
} else if (phase === "writer") {
  oursPresent();
  writerPresent();
  writerIdentity();
} else if (phase === "uninstall") {
  oursAbsent();
  writerPresent();
  // qwen's own non-server key, stamped by mcp list before mcp add runs (0.26.1); uninstall must keep it.
  if (settings["$version"] !== 4) fail("$version (uninstall changed or dropped qwen's version key)");
} else {
  console.error("unknown phase: " + phase);
  process.exit(1);
}
process.exit(bad);
JS

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

# Seed a foreign mcp server, a foreign SessionStart hook, the user's own v2-shaped
# ui keys, and an unknown root key. setup/uninstall must leave every one at its
# seeded path, while our mcp/hook keys legitimately merge beside them. Root `theme`
# is deliberately avoided: qwen's own writer migrates the legacy v1 root key into
# ui.theme, so a root key would read as clobbered through qwen's migration rather
# than a defect.
cat > "$SETTINGS" <<'JSON'
{ "ui": { "theme": "dark", "hideWindowTitle": true, "statusLine": { "type": "command", "command": "their-line", "padding": 2 } }, "mcpServers": { "theirs": { "command": "their-server", "args": [] } }, "theirRootKey": "keep", "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": "their-hook" } ] } ] } }
JSON

host_fixture setup --agent qwen-code

if ! node /usr/local/bin/qwen-settings-check.js setup "$SETTINGS"; then
  echo "FAIL: settings.json after setup is wrong (missing/wrong keys above)"; cat "$SETTINGS"; exit 1
fi
if [ ! -f "$CMD_DIR/hello.md" ];       then echo "FAIL: copied command markdown missing"; exit 1; fi
if [ ! -f "$AGENT" ];                  then echo "FAIL: rendered agent markdown missing"; exit 1; fi
if [ ! -f "$SKILL" ];                  then echo "FAIL: translated skill SKILL.md missing"; exit 1; fi
if ! grep -q 'x-agentgear' "$SKILL";   then echo "FAIL: skill ownership tag missing"; exit 1; fi

# The real CLI ingests the config agentgear wrote: `qwen mcp list` (auth-free at the
# pin) must list our stdio server. A nonzero exit or a missing row fails the leg.
list="$(timeout 120 qwen mcp list)"
if ! printf '%s\n' "$list" | grep -qF 'ez-fixture:'; then
  echo "FAIL: qwen mcp list does not list ez-fixture"; printf '%s\n' "$list"; exit 1
fi

# The harness's OWN native writer against the same settings.json agentgear wrote:
# `qwen mcp add` adds a server of its own beside ours. Its failure fails the leg,
# and `! cmp -s` against the pre-writer copy fails the leg if it wrote nothing.
cp "$SETTINGS" /tmp/settings-pre-writer
qwen mcp add -s user qwen-native-server echo hello
if cmp -s "$SETTINGS" /tmp/settings-pre-writer; then
  echo "FAIL: qwen mcp add wrote nothing (settings.json unchanged)"
  exit 1
fi

# All three mcp entries (the user's, ours, qwen's own) plus every seeded key and
# hook must survive the native writer at their seeded paths, ours by raw identity.
if ! node /usr/local/bin/qwen-settings-check.js writer "$SETTINGS" /tmp/settings-pre-writer; then
  echo "FAIL: settings.json after the native writer is wrong (missing/wrong keys above)"; cat "$SETTINGS"; exit 1
fi

# agentgear's own probe must still read its entries as healthy after the native
# writer; NoOp also proves it rewrites nothing.
cp "$SETTINGS" /tmp/settings-pre-heal
heal="$(printf '%s' '{"source":"startup"}' | host_fixture self-heal)"
if [ "$heal" != "NoOp" ]; then
  echo "FAIL: self-heal after the native writer printed '$heal', want 'NoOp': agentgear no longer reads its own entries as healthy"
  exit 1
fi
if ! cmp -s "$SETTINGS" /tmp/settings-pre-heal; then
  echo "FAIL: self-heal rewrote settings.json"; diff /tmp/settings-pre-heal "$SETTINGS"; exit 1
fi

host_fixture uninstall

# uninstall removes only ours: qwen's own entry and the user's seeded keys stay.
if ! node /usr/local/bin/qwen-settings-check.js uninstall "$SETTINGS"; then
  echo "FAIL: settings.json after uninstall is wrong (missing/wrong keys above)"; cat "$SETTINGS"; exit 1
fi
if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; cat "$SETTINGS"; exit 1; fi
if [ -d "$CMD_DIR" ];                then echo "FAIL: command dir survived uninstall"; exit 1; fi
if [ -f "$AGENT" ];                  then echo "FAIL: agent file survived uninstall"; exit 1; fi
if [ -d "$SKILL_DIR" ];              then echo "FAIL: skill dir survived uninstall"; exit 1; fi

echo "qwen-code docker leg: OK"
TEST
chmod +x /usr/local/bin/qwen-code-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/qwen-code-leg.sh"]
