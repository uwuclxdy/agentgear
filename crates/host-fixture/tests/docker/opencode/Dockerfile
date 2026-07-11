# syntax=docker/dockerfile:1
# opencode docker leg: the real opencode CLI ingesting our written config. Built +
# run by tests/docker/run.sh opencode (context = repo root). Config-file parse is
# the auth-free source of truth; `opencode mcp list` is a best-effort native second
# opinion (documented as a local, no-API-key op, but first-run auth is unverified
# in the brief — see docs/harness/opencode.md).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real opencode CLI + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# `@latest` until a version is verified against a live install; pin + bump it
# alongside docs/harness/opencode.md (the brief could not confirm a version).
RUN npm install -g opencode-ai@latest
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/opencode-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/opencode-home
CONFIG_DIR="$HOME/.config/opencode"
CONFIG="$CONFIG_DIR/opencode.json"
CMD_FILE="$CONFIG_DIR/commands/ez-fixture-plugin-hello.md"
AGENT_FILE="$CONFIG_DIR/agents/ez-fixture-plugin-ez-helper.md"
mkdir -p "$CONFIG_DIR"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$CONFIG" <<'JSON'
{ "theme": "dark", "mcp": { "theirs": { "type": "local", "command": ["their-server"], "enabled": true } } }
JSON

host_fixture setup --agent opencode

if ! grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our mcp server not registered"; cat "$CONFIG"; exit 1; fi
if ! grep -q 'theirs'     "$CONFIG"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'    "$CONFIG"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$CMD_FILE" ];             then echo "FAIL: translated command markdown missing"; exit 1; fi
if [ ! -f "$AGENT_FILE" ];           then echo "FAIL: translated agent markdown missing"; exit 1; fi
if ! grep -q 'mode: subagent' "$AGENT_FILE"; then echo "FAIL: agent not translated to a subagent"; exit 1; fi

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if out=$(opencode mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native opencode mcp list confirmed ez-fixture"
  else
    echo "FAIL: opencode mcp list ran but did not list ez-fixture"; echo "$out"; exit 1
  fi
else
  echo "note: opencode mcp list unavailable (likely needs first-run auth); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$CONFIG"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -f "$CMD_FILE" ];             then echo "FAIL: command file survived uninstall"; exit 1; fi
if [ -f "$AGENT_FILE" ];           then echo "FAIL: agent file survived uninstall"; exit 1; fi

echo "opencode docker leg: OK"
TEST
chmod +x /usr/local/bin/opencode-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/opencode-leg.sh"]
