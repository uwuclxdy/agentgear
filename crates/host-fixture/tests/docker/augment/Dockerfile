# syntax=docker/dockerfile:1
# augment docker leg: the real auggie CLI ingesting our written config. Built + run
# by tests/docker/run.sh augment (context = repo root). Config-file parse is the
# auth-free source of truth; `auggie mcp list` is a best-effort native second opinion
# (whether it runs pre-login is undocumented — see docs/harness/augment.md).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real auggie + the fixture binary; assert the lifecycle end-to-end.
# Node 22+ is required by @augmentcode/auggie.
FROM node:22-slim
# Pin the CLI for reproducibility; bump alongside docs/harness/augment.md.
RUN npm install -g @augmentcode/auggie@0.32.0
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/augment-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/augment-home
SETTINGS="$HOME/.augment/settings.json"
CMD_FILE="$HOME/.augment/commands/ez-fixture-plugin-hello.md"
AGENT_FILE="$HOME/.augment/agents/ez-fixture-plugin-ez-helper.md"
mkdir -p "$HOME/.augment"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$SETTINGS" <<'JSON'
{ "theme": "dark", "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent augment

if ! grep -q 'ez-fixture'  "$SETTINGS"; then echo "FAIL: our mcp server not registered"; cat "$SETTINGS"; exit 1; fi
if ! grep -q 'SessionStart' "$SETTINGS"; then echo "FAIL: SessionStart hook not written"; cat "$SETTINGS"; exit 1; fi
if   grep -q 'check-restart' "$SETTINGS"; then echo "FAIL: UserPromptSubmit hook leaked (no augment analog)"; exit 1; fi
if ! grep -q 'theirs'      "$SETTINGS"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'     "$SETTINGS"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$CMD_FILE" ];              then echo "FAIL: translated command markdown missing"; exit 1; fi
if [ ! -f "$AGENT_FILE" ];            then echo "FAIL: translated agent markdown missing"; exit 1; fi

# Native second opinion (best-effort; the auth-free config parse above is authoritative).
if out=$(auggie mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native auggie mcp list confirmed ez-fixture"
  else
    echo "note: auggie mcp list ran but did not list ez-fixture; relied on config-file assert"; echo "$out"
  fi
else
  echo "note: auggie mcp list unavailable (likely needs login); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$SETTINGS"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -f "$CMD_FILE" ];               then echo "FAIL: command file survived uninstall"; exit 1; fi
if [ -f "$AGENT_FILE" ];             then echo "FAIL: agent file survived uninstall"; exit 1; fi

echo "augment docker leg: OK"
TEST
chmod +x /usr/local/bin/augment-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/augment-leg.sh"]
