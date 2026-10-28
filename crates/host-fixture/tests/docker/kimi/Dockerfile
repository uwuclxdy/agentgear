# syntax=docker/dockerfile:1
# kimi docker leg: the real kimi-code CLI ingesting our written config. Built + run
# by tests/docker/run.sh kimi (context = repo root). The config-file parse is the
# auth-free source of truth; `kimi doctor` (validates config.toml without auth) is a
# native second opinion that fails the leg on a nonzero exit. `kimi mcp list` does
# not exist as a subcommand (confirmed at 0.23.5, 0.26.0, and 2.3.3), so there is no
# native second opinion for mcp — the config-file parse is authoritative. Agent
# profiles DO have one: `kimi --agent <absent>` prints its discovered profile list,
# so the leg asserts kimi's own scan found (and later dropped) our profile.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real kimi-code CLI + the fixture binary; assert the lifecycle end-to-end.
# kimi-code's declared engine floor is >=22.19.0 (npm `engines.node`); node:20 sits
# below it, so bump to a current LTS that clears the floor.
FROM node:22-slim
# Pin the CLI for reproducibility. The scoped
# `@moonshot-ai/kimi-code` is the official package (bin `kimi`); the unscoped
# `kimi-code` on npm is NOT it.
RUN npm install -g @moonshot-ai/kimi-code@2.3.3
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/kimi-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/kimi-home
export KIMI_CODE_HOME="$HOME/.kimi-code"
# CI-friendly kimi env: no telemetry, no background auto-update.
export KIMI_DISABLE_TELEMETRY=1
export KIMI_CODE_NO_AUTO_UPDATE=1
MCP="$KIMI_CODE_HOME/mcp.json"
CONFIG="$KIMI_CODE_HOME/config.toml"
AGENTS="$KIMI_CODE_HOME/AGENTS.md"
SKILL_DIR="$KIMI_CODE_HOME/skills/ez-skill"
SKILL="$SKILL_DIR/SKILL.md"
USER_SKILL="$KIMI_CODE_HOME/skills/mine/SKILL.md"
USER_AGENT="$KIMI_CODE_HOME/agents/mine.md"
AGENT_PROFILE="$KIMI_CODE_HOME/agents/ez-fixture-plugin-ez-helper.md"
MARKERS="$HOME/.local/share/ez-fixture-plugin/markers"
# The stamp file name is the sha256 of plugin name + NUL + scope key + NUL + agent
# (stamp.rs marker_path); pin the one file setup writes, not just any file.
STAMP="$MARKERS/$(printf 'ez-fixture-plugin\0user\0kimi' | sha256sum | cut -d' ' -f1)"
# Pre-create the config dir so the backend's dir-based detect passes (the CLI would
# create it on first run anyway); the two products share the `kimi` binary, so the
# `~/.kimi-code` dir — not the binary name — is what marks this as the current one.
mkdir -p "$KIMI_CODE_HOME" "$KIMI_CODE_HOME/skills/mine" "$KIMI_CODE_HOME/agents"

# Native discovery check for the agent surface: `--agent <absent>` fails before any
# model call and prints kimi's discovered profile names, so membership of that
# parsed list is kimi's own word on whether it loaded a profile we wrote.
# KIMI_MODEL_* synthesizes an in-memory provider (never contacted: profile
# resolution fails first); without one kimi errors on "No model configured" instead.
assert_profile_listed() {
  local out
  if out=$(KIMI_MODEL_NAME=leg-model KIMI_MODEL_API_KEY=leg-key KIMI_MODEL_PROVIDER_TYPE=openai \
      KIMI_MODEL_BASE_URL=http://127.0.0.1:9 kimi --agent __agentgear_absent__ -p hi 2>&1); then
    echo "FAIL: kimi accepted an agent profile that does not exist"; echo "$out"; exit 1
  fi
  printf '%s\n' "$out" > /tmp/kimi-absent-agent.out
  node -e '
const fs = require("fs");
const text = fs.readFileSync(process.argv[1], "utf8");
const want = process.argv[2] === "ours";
const m = /Available profiles: (.*)$/m.exec(text);
if (!m) { console.error("FAIL: no discovered-profile list in the absent-agent error:"); console.error(text); process.exit(1); }
const names = m[1].split(",").map((s) => s.trim());
const has = names.includes("ez-fixture-plugin-ez-helper");
if (has !== want) {
  console.error(`FAIL: ez-fixture-plugin-ez-helper ${want ? "missing from" : "still in"} the discovered profiles: ${names.join(", ")}`);
  process.exit(1);
}
' /tmp/kimi-absent-agent.out "$1"
}

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$MCP" <<'JSON'
{ "theme": "dark", "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

# Seed a foreign hook (colliding event), an untouched-event hook, a key + a comment
# that must survive (comment/key-order preservation is the toml_edit point).
cat > "$CONFIG" <<'TOML'
# the user's own kimi config
model = "kimi-k2"

[[hooks]]
event = "SessionStart"
command = "their-session-hook"

[[hooks]]
event = "Stop"
command = "their-stop-hook"
TOML

# Seed the user's own AGENTS.md guidance and a user-owned skill: uninstall must strip
# only our marker block and our tagged skill, and leave these byte-identical.
printf 'user kimi guidance\n' > "$AGENTS"
cp "$AGENTS" /tmp/kimi-agents-seed
printf '%s\n' '---' 'name: mine' 'description: user skill' '---' '' 'mine' > "$USER_SKILL"
cp "$USER_SKILL" /tmp/kimi-skill-seed
printf '%s\n' '---' 'name: mine' 'description: a user-owned agent profile' '---' '' 'theirs' > "$USER_AGENT"
cp "$USER_AGENT" /tmp/kimi-agent-seed

host_fixture setup --agent kimi

# Our mcp servers must be present by exact key, and our stdio server by content
# (command + args), never a substring that would also match ez-fixture-http/-sse.
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const j = JSON.parse(raw);
const fail = (m) => { console.error(m); console.error(raw); process.exit(1); };
const servers = j.mcpServers || {};
const miss = ["ez-fixture", "ez-fixture-http", "ez-fixture-sse"].filter((k) => !(k in servers));
if (miss.length) fail("FAIL: mcp servers missing: " + miss.join(", "));
const s = servers["ez-fixture"] || {};
if (s.command !== "host_fixture" || JSON.stringify(s.args) !== JSON.stringify(["mcp"])) {
  fail("FAIL: our stdio server is not host_fixture mcp: " + JSON.stringify(s));
}
const theirs = servers["theirs"] || {};
if (j.theme !== "dark" || theirs.command !== "their-server" || JSON.stringify(theirs.args) !== JSON.stringify([])) {
  fail("FAIL: seeded theme or theirs server changed: " + JSON.stringify({ theme: j.theme, theirs }));
}
' "$MCP"

# Hooks are [[hooks]] tables in config.toml: assert each of ours by event AND command,
# and that the seeded foreign hooks survive. Each block ends at the next table header,
# the event and command regexes are anchored to the start of a line inside that table
# (so `prevent =` cannot match), and both accept single-quoted TOML strings.
node -e '
const fs = require("fs");
const text = fs.readFileSync(process.argv[1], "utf8");
const fail = (m) => { console.error(m); console.error(text); process.exit(1); };
const blocks = text.split(/^[ \t]*\[\[hooks\]\][ \t]*(?:#.*)?$/m).slice(1).map((b) => b.split(/^[ \t]*\[/m)[0]);
const hooks = blocks.map((b) => {
  const ev = /^[ \t]*event[ \t]*=[ \t]*(?:"([^"]*)"|\x27([^\x27]*)\x27)/m.exec(b);
  const cmd = /^[ \t]*command[ \t]*=[ \t]*(?:"([^"]*)"|\x27([^\x27]*)\x27)/m.exec(b);
  return { event: ev ? (ev[1] !== undefined ? ev[1] : ev[2]) : null, command: cmd ? (cmd[1] !== undefined ? cmd[1] : cmd[2]) : null };
});
const find = (ev, cmd) => hooks.some((h) => h.event === ev && h.command === cmd);
const want = { SessionStart: "host_fixture self-heal", UserPromptSubmit: "host_fixture check-restart" };
const missHooks = Object.keys(want).filter((ev) => !find(ev, want[ev]));
if (missHooks.length) fail("FAIL: hooks missing: " + missHooks.join(", "));
if (!find("SessionStart", "their-session-hook")) fail("FAIL: seeded colliding hook clobbered on install");
if (!find("Stop", "their-stop-hook")) fail("FAIL: seeded untouched hook clobbered on install");
' "$CONFIG"
if ! grep -q 'kimi-k2'            "$CONFIG"; then echo "FAIL: seeded config key clobbered on install"; exit 1; fi
if ! grep -q "user's own kimi config" "$CONFIG"; then echo "FAIL: seeded comment dropped on install"; exit 1; fi
if [ ! -f "$SKILL" ];             then echo "FAIL: translated skill SKILL.md missing"; exit 1; fi
if ! grep -q 'x-agentgear' "$SKILL"; then echo "FAIL: skill ownership tag missing"; exit 1; fi
if [ ! -f "$SKILL_DIR/reference.md" ]; then echo "FAIL: skill support file reference.md missing"; exit 1; fi
if ! cmp -s "$USER_SKILL" /tmp/kimi-skill-seed; then echo "FAIL: seeded skill clobbered on install"; exit 1; fi
# The agent profile must parse under kimi's own rules (kebab name, description, no
# model key) and carry the CC prompt; the user's own profile survives untouched.
if [ ! -f "$AGENT_PROFILE" ]; then echo "FAIL: translated agent profile missing"; exit 1; fi
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const fail = (m) => { console.error(m); console.error(raw); process.exit(1); };
const m = /^---\n([\s\S]*?)\n---\n\n([\s\S]*)$/.exec(raw);
if (!m) fail("FAIL: agent profile has no frontmatter block");
const fields = {};
for (const line of m[1].split("\n")) {
  const i = line.indexOf(": ");
  if (i !== -1) fields[line.slice(0, i)] = line.slice(i + 2).replace(/^"(.*)"$/, "$1");
}
if (fields.name !== "ez-fixture-plugin-ez-helper") fail("FAIL: profile name is not the plugin-prefixed kebab name: " + fields.name);
if (!fields.description || !fields.description.startsWith("fixture subagent")) fail("FAIL: profile description missing: " + fields.description);
if ("model" in fields) fail("FAIL: a model key reached the profile (kimi has no agent model field)");
if (!m[2].startsWith("A fixture helper agent.")) fail("FAIL: profile body is not the CC agent prompt");
' "$AGENT_PROFILE"
if ! cmp -s "$USER_AGENT" /tmp/kimi-agent-seed; then echo "FAIL: seeded agent profile clobbered on install"; exit 1; fi
assert_profile_listed ours
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const lines = raw.split("\n");
const begin = lines.indexOf("<!-- agentgear:ez-fixture-plugin@ez-fixture-plugin begin -->");
const body  = lines.indexOf("ez-fixture always-loaded guidance line.");
const end   = lines.indexOf("<!-- agentgear:ez-fixture-plugin@ez-fixture-plugin end -->");
if (begin === -1 || body === -1 || end === -1 || !(begin < body && body < end)) {
  console.error("FAIL: our AGENTS.md guidance block missing begin/body/end in order");
  console.error(raw);
  process.exit(1);
}
' "$AGENTS"
if ! grep -q 'user kimi guidance' "$AGENTS"; then
  echo "FAIL: seeded AGENTS.md line lost on install"; cat "$AGENTS"; exit 1
fi
if [ ! -f "$STAMP" ]; then echo "FAIL: stamp marker missing after setup"; exit 1; fi

# Native second opinion: `kimi doctor` validates config.toml/tui.toml without auth
# (no native `kimi mcp list` exists, so mcp correctness rests on the asserts above).
# A missing CLI is a broken pinned install; a nonzero exit fails.
if ! command -v kimi >/dev/null 2>&1; then
  echo "FAIL: kimi not on PATH (pinned install missing)"; exit 1
elif out=$(kimi doctor 2>&1); then
  echo "native kimi doctor ran (config validated without auth)"
else
  rc=$?
  echo "$out"
  echo "FAIL: kimi doctor exited $rc"; exit 1
fi

host_fixture uninstall

# Our entries must be gone; the seeded server/key/hook/comment/skill and the user's
# own AGENTS.md lines must survive.
if grep -q 'ez-fixture' "$MCP"; then echo "FAIL: our server survived uninstall"; exit 1; fi
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const j = JSON.parse(raw);
const fail = (m) => { console.error(m); console.error(raw); process.exit(1); };
const servers = j.mcpServers || {};
const theirs = servers["theirs"] || {};
if (j.theme !== "dark" || theirs.command !== "their-server" || JSON.stringify(theirs.args) !== JSON.stringify([])) {
  fail("FAIL: uninstall changed the seeded theme or theirs server: " + JSON.stringify({ theme: j.theme, theirs }));
}
' "$MCP"
node -e '
const fs = require("fs");
const text = fs.readFileSync(process.argv[1], "utf8");
const fail = (m) => { console.error(m); console.error(text); process.exit(1); };
const blocks = text.split(/^[ \t]*\[\[hooks\]\][ \t]*(?:#.*)?$/m).slice(1).map((b) => b.split(/^[ \t]*\[/m)[0]);
const hooks = blocks.map((b) => {
  const ev = /^[ \t]*event[ \t]*=[ \t]*(?:"([^"]*)"|\x27([^\x27]*)\x27)/m.exec(b);
  const cmd = /^[ \t]*command[ \t]*=[ \t]*(?:"([^"]*)"|\x27([^\x27]*)\x27)/m.exec(b);
  return { event: ev ? (ev[1] !== undefined ? ev[1] : ev[2]) : null, command: cmd ? (cmd[1] !== undefined ? cmd[1] : cmd[2]) : null };
});
const find = (ev, cmd) => hooks.some((h) => h.event === ev && h.command === cmd);
const ours = { SessionStart: "host_fixture self-heal", UserPromptSubmit: "host_fixture check-restart" };
const leftHooks = Object.keys(ours).filter((ev) => find(ev, ours[ev]));
if (leftHooks.length) fail("FAIL: our hooks survived uninstall: " + leftHooks.join(", "));
if (!find("SessionStart", "their-session-hook")) fail("FAIL: uninstall removed the seeded SessionStart hook");
if (!find("Stop", "their-stop-hook")) fail("FAIL: uninstall removed the seeded Stop hook");
' "$CONFIG"
if grep -q 'host_fixture self-heal'   "$CONFIG"; then echo "FAIL: our SessionStart hook survived uninstall"; exit 1; fi
if grep -q 'host_fixture check-restart' "$CONFIG"; then echo "FAIL: our UserPromptSubmit hook survived uninstall"; exit 1; fi
if ! grep -q 'kimi-k2'            "$CONFIG"; then echo "FAIL: uninstall removed the seeded config key"; exit 1; fi
if ! grep -q "user's own kimi config" "$CONFIG"; then echo "FAIL: uninstall dropped the seeded comment"; exit 1; fi
if [ -d "$SKILL_DIR" ]; then echo "FAIL: skill dir survived uninstall"; exit 1; fi
if ! cmp -s "$USER_SKILL" /tmp/kimi-skill-seed; then echo "FAIL: uninstall removed or rewrote the seeded skill"; exit 1; fi
if [ -f "$AGENT_PROFILE" ]; then echo "FAIL: our agent profile survived uninstall"; exit 1; fi
if ! cmp -s "$USER_AGENT" /tmp/kimi-agent-seed; then echo "FAIL: uninstall removed or rewrote the seeded agent profile"; exit 1; fi
assert_profile_listed absent
if grep -q 'agentgear:ez-fixture-plugin@ez-fixture-plugin' "$AGENTS"; then
  echo "FAIL: our AGENTS.md block survived uninstall"; cat "$AGENTS"; exit 1
fi
if ! cmp -s "$AGENTS" /tmp/kimi-agents-seed; then
  echo "FAIL: uninstall rewrote the user's own AGENTS.md lines"; diff /tmp/kimi-agents-seed "$AGENTS"; exit 1
fi
if [ -e "$STAMP" ]; then echo "FAIL: stamp marker survived uninstall"; exit 1; fi

# Native re-run: after uninstall kimi doctor must still accept the config; doctor
# names no server or hook, so absence rests on the config asserts above.
if ! command -v kimi >/dev/null 2>&1; then
  echo "FAIL: kimi not on PATH after uninstall"; exit 1
fi
if out=$(kimi doctor 2>&1); then
  echo "native kimi doctor accepted the config post-uninstall"
else
  rc=$?
  echo "$out"
  echo "FAIL: kimi doctor exited $rc after uninstall"; exit 1
fi

echo "kimi docker leg: OK"
TEST
chmod +x /usr/local/bin/kimi-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/kimi-leg.sh"]
