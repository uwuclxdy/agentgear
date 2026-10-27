# syntax=docker/dockerfile:1
# gemini docker leg: the real gemini-cli ingesting our written config. Built +
# run by tests/docker/run.sh gemini (context = repo root). Config-file parse is the
# auth-free source of truth; `gemini mcp list` is a native second opinion that fails
# the leg on a nonzero exit or a missing row.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real gemini-cli + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# Pin the CLI for reproducibility.
RUN npm install -g @google/gemini-cli@0.66.0
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/gemini-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/gemini-home
SETTINGS="$HOME/.gemini/settings.json"
CMD_DIR="$HOME/.gemini/commands/ez-fixture-plugin"
AGENT_FILE="$HOME/.gemini/agents/ez-fixture-plugin-ez-helper.md"
USER_CMD="$HOME/.gemini/commands/mine.toml"
USER_AGENT="$HOME/.gemini/agents/mine.md"
GMD="$HOME/.gemini/GEMINI.md"
MARKERS="$HOME/.local/share/ez-fixture-plugin/markers"
# The stamp file name is the sha256 of plugin name + NUL + scope key + NUL + agent
# (stamp.rs marker_path); pin the one file setup writes, not just any file.
STAMP="$MARKERS/$(printf 'ez-fixture-plugin\0user\0gemini' | sha256sum | cut -d' ' -f1)"
mkdir -p "$HOME/.gemini" "$HOME/.gemini/commands" "$HOME/.gemini/agents"

# Seed a foreign mcp server, a colliding SessionStart hook, a user-owned command and
# agent, and an unrelated key that must all survive install/uninstall.
cat > "$SETTINGS" <<'JSON'
{ "theme": "dark", "mcpServers": { "theirs": { "command": "their-server", "args": [] } },
  "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": "their-session-hook" } ] } ] } }
JSON
printf 'prompt = "mine"\n' > "$USER_CMD"
printf '%s\n' '---' 'name: mine' 'description: user agent' '---' '' 'mine' > "$USER_AGENT"
cp "$USER_CMD"   /tmp/gemini-user-cmd-seed
cp "$USER_AGENT" /tmp/gemini-user-agent-seed

# Seed the user's own GEMINI.md guidance: uninstall must strip only our marker block
# and leave these lines byte-identical.
printf 'user gemini guidance\n' > "$GMD"
cp "$GMD" /tmp/gemini-user-seed

host_fixture setup --agent gemini

# Our mcp servers and hooks must be present by exact key, and our stdio server by
# content (command + args), never a substring that would also match
# ez-fixture-http/-sse.
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const j = JSON.parse(raw);
const fail = (m) => { console.error(m); console.error(raw); process.exit(1); };
const servers = j.mcpServers || {};
const hooks = j.hooks || {};
const missMcp = ["ez-fixture", "ez-fixture-http", "ez-fixture-sse"].filter((k) => !(k in servers));
if (missMcp.length) fail("FAIL: mcp servers missing: " + missMcp.join(", "));
const cmds = (ev) => (hooks[ev] || []).flatMap((g) => (g.hooks || []).map((h) => h.command));
const want = { SessionStart: "host_fixture self-heal", BeforeAgent: "host_fixture check-restart" };
const missHooks = Object.keys(want).filter((ev) => !cmds(ev).includes(want[ev]));
if (missHooks.length) fail("FAIL: hooks missing: " + missHooks.join(", "));
const s = servers["ez-fixture"] || {};
if (s.command !== "host_fixture" || JSON.stringify(s.args) !== JSON.stringify(["mcp"])) {
  fail("FAIL: our stdio server is not host_fixture mcp: " + JSON.stringify(s));
}
const theirs = servers["theirs"] || {};
if (j.theme !== "dark" || theirs.command !== "their-server" || JSON.stringify(theirs.args) !== JSON.stringify([])) {
  fail("FAIL: seeded theme or theirs server changed: " + JSON.stringify({ theme: j.theme, theirs }));
}
if (!cmds("SessionStart").includes("their-session-hook")) fail("FAIL: seeded SessionStart hook clobbered on install");
' "$SETTINGS"
if [ ! -f "$CMD_DIR/hello.toml" ]; then echo "FAIL: translated command TOML missing"; exit 1; fi
if [ ! -f "$AGENT_FILE" ];          then echo "FAIL: translated agent markdown missing"; exit 1; fi
if ! cmp -s "$USER_CMD"   /tmp/gemini-user-cmd-seed;   then echo "FAIL: seeded command file clobbered on install"; exit 1; fi
if ! cmp -s "$USER_AGENT" /tmp/gemini-user-agent-seed; then echo "FAIL: seeded agent file clobbered on install"; exit 1; fi
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const lines = raw.split("\n");
const begin = lines.indexOf("<!-- agentgear:ez-fixture-plugin@ez-fixture-plugin begin -->");
const body  = lines.indexOf("ez-fixture always-loaded guidance line.");
const end   = lines.indexOf("<!-- agentgear:ez-fixture-plugin@ez-fixture-plugin end -->");
if (begin === -1 || body === -1 || end === -1 || !(begin < body && body < end)) {
  console.error("FAIL: our GEMINI.md guidance block missing begin/body/end in order");
  console.error(raw);
  process.exit(1);
}
' "$GMD"
if ! grep -q 'user gemini guidance' "$GMD"; then
  echo "FAIL: seeded GEMINI.md line lost on install"; cat "$GMD"; exit 1
fi
if [ ! -f "$STAMP" ]; then echo "FAIL: stamp marker missing after setup"; exit 1; fi

# Native second opinion: proves gemini parsed our settings.json and lists our stdio
# server (cwd / is untrusted, so it never connects; the servers read Disabled). A
# missing CLI is a broken pinned install; a nonzero exit or a missing row fails.
if ! command -v gemini >/dev/null 2>&1; then
  echo "FAIL: gemini not on PATH (pinned install missing)"; exit 1
elif out=$(gemini mcp list 2>&1); then
  if grep -qE '(^|[^[:alnum:]-])ez-fixture([^[:alnum:]-]|$)' <<<"$out"; then
    echo "native gemini mcp list confirmed ez-fixture"
  else
    echo "$out"
    echo "FAIL: gemini mcp list ran but did not list ez-fixture"; exit 1
  fi
else
  rc=$?
  echo "$out"
  echo "FAIL: gemini mcp list exited $rc"; exit 1
fi

host_fixture uninstall

# Our entries must be gone by exact key and by event + command; the seeded
# server/key/hook, the user's own command, agent and GEMINI.md lines must survive.
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const j = JSON.parse(raw);
const fail = (m) => { console.error(m); console.error(raw); process.exit(1); };
const servers = j.mcpServers || {};
const hooks = j.hooks || {};
const leftMcp = ["ez-fixture", "ez-fixture-http", "ez-fixture-sse"].filter((k) => k in servers);
if (leftMcp.length) fail("FAIL: our mcp servers survived uninstall: " + leftMcp.join(", "));
const cmds = (ev) => (hooks[ev] || []).flatMap((g) => (g.hooks || []).map((h) => h.command));
const want = { SessionStart: "host_fixture self-heal", BeforeAgent: "host_fixture check-restart" };
const leftHooks = Object.keys(want).filter((ev) => cmds(ev).includes(want[ev]));
if (leftHooks.length) fail("FAIL: our hooks survived uninstall: " + leftHooks.join(", "));
if (!cmds("SessionStart").includes("their-session-hook")) fail("FAIL: uninstall removed the seeded SessionStart hook");
const theirs = servers["theirs"] || {};
if (j.theme !== "dark" || theirs.command !== "their-server" || JSON.stringify(theirs.args) !== JSON.stringify([])) {
  fail("FAIL: uninstall changed the seeded theme or theirs server: " + JSON.stringify({ theme: j.theme, theirs }));
}
' "$SETTINGS"
if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; cat "$SETTINGS"; exit 1; fi
if [ -d "$CMD_DIR" ];    then echo "FAIL: command dir survived uninstall"; exit 1; fi
if [ -f "$AGENT_FILE" ]; then echo "FAIL: agent file survived uninstall"; exit 1; fi
if ! cmp -s "$USER_CMD"   /tmp/gemini-user-cmd-seed;   then echo "FAIL: uninstall removed or rewrote the seeded command file"; exit 1; fi
if ! cmp -s "$USER_AGENT" /tmp/gemini-user-agent-seed; then echo "FAIL: uninstall removed or rewrote the seeded agent file"; exit 1; fi
if grep -q 'agentgear:ez-fixture-plugin@ez-fixture-plugin' "$GMD"; then
  echo "FAIL: our GEMINI.md block survived uninstall"; cat "$GMD"; exit 1
fi
if ! cmp -s "$GMD" /tmp/gemini-user-seed; then
  echo "FAIL: uninstall rewrote the user's own GEMINI.md lines"; diff /tmp/gemini-user-seed "$GMD"; exit 1
fi
if [ -e "$STAMP" ]; then echo "FAIL: stamp marker survived uninstall"; exit 1; fi

# Native re-run: after uninstall gemini must still read its config and no longer list
# ez-fixture.
if ! command -v gemini >/dev/null 2>&1; then
  echo "FAIL: gemini not on PATH after uninstall"; exit 1
fi
if out=$(gemini mcp list 2>&1); then
  if grep -qE '(^|[^[:alnum:]-])ez-fixture([^[:alnum:]-]|$)' <<<"$out"; then
    echo "$out"
    echo "FAIL: gemini mcp list still lists ez-fixture after uninstall"; exit 1
  fi
  echo "native gemini mcp list clean post-uninstall"
else
  rc=$?
  echo "$out"
  echo "FAIL: gemini mcp list exited $rc after uninstall"; exit 1
fi

echo "gemini docker leg: OK"
TEST
chmod +x /usr/local/bin/gemini-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/gemini-leg.sh"]
