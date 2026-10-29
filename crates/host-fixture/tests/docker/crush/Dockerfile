# syntax=docker/dockerfile:1
# crush docker leg: the real charmbracelet/crush ingesting our written config.
# Built + run by tests/docker/run.sh crush (context = repo root). Crush has no
# auth-free `mcp list` (`crush run` needs provider creds), so the config-file parse is
# the authoritative source of truth; `crush dirs` (resolves the config root) and
# `crush models` (decodes crush.json) are native second opinions that fail the leg on
# a missing CLI, a nonzero exit, or a missing expected line.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real crush CLI + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# Pin the CLI for reproducibility; a failed install fails the leg.
RUN npm install -g @charmland/crush@0.99.0
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/crush-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/crush-home
# No CRUSH_GLOBAL_CONFIG: exercise the natural XDG path both our backend and the
# real crush resolve to ($XDG_CONFIG_HOME defaults to $HOME/.config).
CONFIG="$HOME/.config/crush/crush.json"
SKILL_DIR="$HOME/.config/crush/skills/ez-skill"
SKILL="$SKILL_DIR/SKILL.md"
CMD_DIR="$HOME/.config/crush/commands/ez-fixture-plugin"
CMD="$CMD_DIR/hello.md"
CRUSH_MD="$HOME/.config/crush/CRUSH.md"
MARKERS="$HOME/.local/share/ez-fixture-plugin/markers"
# The stamp file name is the sha256 of plugin name + NUL + scope key + NUL + agent
# (stamp.rs marker_path); pin the one file setup writes, not just any file.
STAMP="$MARKERS/$(printf 'ez-fixture-plugin\0user\0crush' | sha256sum | cut -d' ' -f1)"
mkdir -p "$(dirname "$CONFIG")"

if ! command -v crush >/dev/null 2>&1; then
  echo "FAIL: crush not on PATH (pinned install missing)"; exit 1
fi
# A native check that cannot fail proves nothing: crush must reject a corrupt
# crush.json before its later green counts (v0.98.2 exits 1 on invalid JSON).
printf '{ not json' > "$CONFIG"
if out=$(crush models 2>&1); then
  echo "$out"
  echo "FAIL: crush models accepted an invalid crush.json; the native config check is vacuous"; exit 1
fi

# Seed a foreign mcp server, a user PreToolUse hook, and an unrelated key that must
# survive install/uninstall.
cat > "$CONFIG" <<'JSON'
{ "options": { "debug": false }, "mcp": { "theirs": { "type": "stdio", "command": "their-server", "args": [], "env": {} } }, "hooks": { "PreToolUse": [ { "command": "their-guard.sh" } ] } }
JSON
# The user's own CRUSH.md line must survive install and uninstall byte-identical.
printf 'user own rule\n' > "$CRUSH_MD"
cp "$CRUSH_MD" /tmp/crush-user-seed

host_fixture setup --agent crush

# Our mcp servers must be present by exact key and by exact shape (type, command,
# url, args), never a substring that would also match ez-fixture-http/-sse; the
# seeded server, key and hook must survive the same merge.
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const j = JSON.parse(raw);
const fail = (m) => { console.error(m); console.error(raw); process.exit(1); };
const canon = (v) => Array.isArray(v) ? v.map(canon)
  : (v && typeof v === "object") ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, canon(v[k])])) : v;
const eq = (a, b) => JSON.stringify(canon(a)) === JSON.stringify(canon(b));
const servers = j.mcp || {};
const ourKeys = ["ez-fixture", "ez-fixture-http", "ez-fixture-sse"];
const missMcp = ourKeys.filter((k) => !(k in servers));
if (missMcp.length) fail("FAIL: mcp servers missing: " + missMcp.join(", "));
const want = {
  "ez-fixture": { type: "stdio", command: "host_fixture", args: ["mcp"], env: {} },
  "ez-fixture-http": { type: "http", url: "http://127.0.0.1:39621/mcp", headers: {} },
  "ez-fixture-sse": { type: "sse", url: "http://127.0.0.1:39622/sse", headers: {} },
};
for (const k of Object.keys(want)) {
  if (!eq(servers[k], want[k])) fail("FAIL: " + k + " is not the expected entry: " + JSON.stringify(servers[k]));
}
if (!eq(j.options, { debug: false })) fail("FAIL: seeded options changed: " + JSON.stringify(j.options));
if (!eq(servers.theirs, { type: "stdio", command: "their-server", args: [], env: {} })) {
  fail("FAIL: seeded server changed: " + JSON.stringify(servers.theirs));
}
if (!eq(j.hooks, { PreToolUse: [{ command: "their-guard.sh" }] })) {
  fail("FAIL: seeded PreToolUse hook changed: " + JSON.stringify(j.hooks));
}
' "$CONFIG"
if [ ! -f "$SKILL" ]; then echo "FAIL: translated skill SKILL.md missing"; exit 1; fi
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const fail = (m) => { console.error(m); console.error(raw); process.exit(1); };
const lines = raw.split("\n");
if (lines[0] !== "---") fail("FAIL: skill frontmatter fence missing");
const end = lines.indexOf("---", 1);
if (end === -1) fail("FAIL: skill frontmatter unterminated");
const fm = lines.slice(1, end);
if (!fm.some((l) => l === "name: ez-skill")) fail("FAIL: skill name missing from frontmatter");
if (!fm.some((l) => l.startsWith("description:"))) fail("FAIL: skill description missing from frontmatter");
const tag = fm.find((l) => l.startsWith("x-agentgear:"));
const tagValue = tag && tag.slice("x-agentgear:".length).trim().replace(/^"|"$/g, "");
if (tagValue !== "ez-fixture-plugin@ez-fixture-plugin") {
  fail("FAIL: skill ownership tag missing or wrong: " + JSON.stringify(tag));
}
' "$SKILL"
if [ ! -f "$CMD" ]; then echo "FAIL: translated command file missing"; exit 1; fi
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const want = "Say hello. This command exists only to give the fixture plugin a component.\n";
if (raw !== want) {
  console.error("FAIL: command body is not the translated body byte-for-byte (CC frontmatter leaked?)");
  console.error(JSON.stringify(raw));
  process.exit(1);
}
' "$CMD"
if [ ! -f "$STAMP" ]; then echo "FAIL: stamp marker missing after setup"; exit 1; fi
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const want = "user own rule\n\n<!-- agentgear:ez-fixture-plugin@ez-fixture-plugin begin -->\nez-fixture always-loaded guidance line.\n<!-- agentgear:ez-fixture-plugin@ez-fixture-plugin end -->\n";
if (raw !== want) {
  console.error("FAIL: CRUSH.md is not exactly the user line plus our tagged block");
  console.error(JSON.stringify(raw));
  process.exit(1);
}
' "$CRUSH_MD"

# Native check: `crush dirs` must resolve the config root we write (a moved root
# would leave our config unread while a models-only check still greens), and
# `crush models` must decode the written crush.json (a wrong-typed field exits
# nonzero). The captured strings go in by here-string: under pipefail an early-exit
# `grep -q` in a pipe can SIGPIPE the producer and flip a negative check.
if out=$(crush dirs 2>&1); then
  mapfile -t dir_lines <<<"$out"
  if [ "${dir_lines[0]}" = "$HOME/.config/crush" ]; then
    echo "native crush dirs resolved $HOME/.config/crush"
  else
    echo "$out"
    echo "FAIL: crush dirs config root is not $HOME/.config/crush"; exit 1
  fi
else
  rc=$?
  echo "$out"
  echo "FAIL: crush dirs exited $rc"; exit 1
fi
if out=$(crush models 2>&1); then
  if [ -n "$out" ]; then
    echo "native crush models loaded the written crush.json"
  else
    echo "FAIL: crush models listed no models"; exit 1
  fi
else
  rc=$?
  echo "$out"
  echo "FAIL: crush models exited $rc (crush could not load $CONFIG)"; exit 1
fi

host_fixture uninstall

# Our entries must be gone by exact key; the seeded server/key/hook and the user's
# own CRUSH.md line must survive byte-identical.
node -e '
const fs = require("fs");
const raw = fs.readFileSync(process.argv[1], "utf8");
const j = JSON.parse(raw);
const fail = (m) => { console.error(m); console.error(raw); process.exit(1); };
const canon = (v) => Array.isArray(v) ? v.map(canon)
  : (v && typeof v === "object") ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, canon(v[k])])) : v;
const eq = (a, b) => JSON.stringify(canon(a)) === JSON.stringify(canon(b));
const servers = j.mcp || {};
const leftMcp = ["ez-fixture", "ez-fixture-http", "ez-fixture-sse"].filter((k) => k in servers);
if (leftMcp.length) fail("FAIL: our mcp servers survived uninstall: " + leftMcp.join(", "));
if (!eq(servers.theirs, { type: "stdio", command: "their-server", args: [], env: {} })) {
  fail("FAIL: uninstall changed or removed the seeded server: " + JSON.stringify(servers.theirs));
}
if (!eq(j.options, { debug: false })) fail("FAIL: uninstall changed or removed the seeded options: " + JSON.stringify(j.options));
if (!eq(j.hooks, { PreToolUse: [{ command: "their-guard.sh" }] })) {
  fail("FAIL: uninstall changed or removed the seeded PreToolUse hook: " + JSON.stringify(j.hooks));
}
' "$CONFIG"
if grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our server survived uninstall"; cat "$CONFIG"; exit 1; fi
if [ -d "$SKILL_DIR" ]; then echo "FAIL: skill dir survived uninstall"; exit 1; fi
if [ -d "$CMD_DIR" ]; then echo "FAIL: command dir survived uninstall"; exit 1; fi
if [ -e "$STAMP" ]; then echo "FAIL: stamp marker survived uninstall"; exit 1; fi
if ! cmp -s "$CRUSH_MD" /tmp/crush-user-seed; then
  echo "FAIL: uninstall did not restore the user's CRUSH.md byte-for-byte"; diff /tmp/crush-user-seed "$CRUSH_MD"; exit 1
fi

# Native re-run: the remaining config must still decode once our entries are gone.
if ! command -v crush >/dev/null 2>&1; then
  echo "FAIL: crush not on PATH after uninstall"; exit 1
fi
if out=$(crush models 2>&1); then
  echo "native crush models still loads the config post-uninstall"
else
  rc=$?
  echo "$out"
  echo "FAIL: crush models exited $rc after uninstall"; exit 1
fi

echo "crush docker leg: OK"
TEST
chmod +x /usr/local/bin/crush-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/crush-leg.sh"]
