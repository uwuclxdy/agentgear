# syntax=docker/dockerfile:1
# crush docker leg: the real charmbracelet/crush ingesting our written config.
# Built + run by tests/docker/run.sh crush (context = repo root). Crush has no
# auth-free native `mcp list` (`crush run` needs provider creds), so the config-file
# parse is the authoritative source of truth; `crush dirs` is a best-effort native
# second opinion that it discovers the same crush.json path we write.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real crush CLI + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# The npm wrapper is auth-free; pin alongside docs/harness/crush.md when a version
# is chosen. Best-effort: the config-file parse below is authoritative, so a CLI
# that fails to install must not fail the leg (the hermetic test owns correctness).
RUN npm install -g @charmland/crush || echo "note: crush CLI unavailable; relying on config-file assert"
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
mkdir -p "$(dirname "$CONFIG")"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$CONFIG" <<'JSON'
{ "options": { "debug": false }, "mcp": { "theirs": { "type": "stdio", "command": "their-server", "args": [], "env": {} } } }
JSON

host_fixture setup --agent crush

if ! grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our mcp server not registered"; cat "$CONFIG"; exit 1; fi
if ! grep -q 'theirs'     "$CONFIG"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"options"'  "$CONFIG"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$SKILL" ];                then echo "FAIL: translated skill SKILL.md missing"; exit 1; fi
if ! grep -q 'x-agentgear' "$SKILL"; then echo "FAIL: skill ownership tag missing"; exit 1; fi

# Confirm the written JSON actually parses (node is present in this stage).
node -e 'JSON.parse(require("fs").readFileSync(process.env.CONFIG,"utf8"))' \
  || { echo "FAIL: crush.json does not parse"; exit 1; }

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if out=$(crush dirs 2>&1); then
  if echo "$out" | grep -q 'crush.json'; then
    echo "native crush dirs discovered our crush.json"
  else
    echo "note: crush dirs ran but did not list crush.json; relied on config-file assert"
  fi
else
  echo "note: crush dirs unavailable; relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$CONFIG"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if ! grep -q '"options"' "$CONFIG"; then echo "FAIL: uninstall removed the seeded key"; exit 1; fi
if [ -d "$SKILL_DIR" ]; then echo "FAIL: skill dir survived uninstall"; exit 1; fi

echo "crush docker leg: OK"
TEST
chmod +x /usr/local/bin/crush-leg.sh
SH

ENV CONFIG=/tmp/crush-home/.config/crush/crush.json
ENTRYPOINT ["/usr/local/bin/crush-leg.sh"]
