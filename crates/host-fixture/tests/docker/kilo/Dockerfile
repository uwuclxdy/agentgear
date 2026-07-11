# syntax=docker/dockerfile:1
# kilo docker leg: the real Kilo Code CLI (`@kilocode/cli`) ingesting our written
# config. Built + run by tests/docker/run.sh kilo (context = repo root). Config-file
# parse is the auth-free source of truth; `kilo mcp list` is a best-effort native
# second opinion (documented as a local config op, but a login requirement for it is
# unconfirmed by the docs — see docs/harness/kilo.md).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real Kilo Code CLI + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# `@latest` until a version is verified against a live install; pin + bump it
# alongside docs/harness/kilo.md (the brief could not confirm a released version).
RUN npm install -g @kilocode/cli@latest
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/kilo-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/kilo-home
CONFIG_DIR="$HOME/.config/kilo"
CONFIG="$CONFIG_DIR/kilo.json"
CMD_FILE="$CONFIG_DIR/commands/ez-fixture-plugin-hello.md"
AGENT_FILE="$CONFIG_DIR/agents/ez-fixture-plugin-ez-helper.md"
mkdir -p "$CONFIG_DIR"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$CONFIG" <<'JSON'
{ "theme": "dark", "mcp": { "theirs": { "type": "local", "command": ["their-server"], "enabled": true } } }
JSON

host_fixture setup --agent kilo

if ! grep -q 'ez-fixture'     "$CONFIG"; then echo "FAIL: our mcp server not registered"; cat "$CONFIG"; exit 1; fi
if ! grep -q '"local"'        "$CONFIG"; then echo "FAIL: opencode-forked type:local shape missing"; exit 1; fi
if ! grep -q '"environment"'  "$CONFIG"; then echo "FAIL: kilo environment field missing"; exit 1; fi
if ! grep -q 'theirs'         "$CONFIG"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'        "$CONFIG"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$CMD_FILE" ];                  then echo "FAIL: translated command markdown missing"; exit 1; fi
if [ ! -f "$AGENT_FILE" ];                then echo "FAIL: translated agent markdown missing"; exit 1; fi
if ! grep -q 'mode: subagent' "$AGENT_FILE"; then echo "FAIL: agent not translated to a subagent"; exit 1; fi

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if out=$(kilo mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native kilo mcp list confirmed ez-fixture"
  else
    echo "FAIL: kilo mcp list ran but did not list ez-fixture"; echo "$out"; exit 1
  fi
else
  echo "note: kilo mcp list unavailable (likely needs login); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$CONFIG"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -f "$CMD_FILE" ];             then echo "FAIL: command file survived uninstall"; exit 1; fi
if [ -f "$AGENT_FILE" ];           then echo "FAIL: agent file survived uninstall"; exit 1; fi

echo "kilo docker leg: OK"
TEST
chmod +x /usr/local/bin/kilo-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/kilo-leg.sh"]
