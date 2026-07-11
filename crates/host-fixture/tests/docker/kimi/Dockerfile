# syntax=docker/dockerfile:1
# kimi docker leg: the real kimi-code CLI ingesting our written config. Built + run
# by tests/docker/run.sh kimi (context = repo root). The config-file parse is the
# auth-free source of truth; `kimi doctor` (validates config without auth) and
# `kimi mcp list` are best-effort native second opinions — running kimi needs an
# interactive /login, so the live `mcp` listing may be unavailable (see
# docs/harness/kimi.md).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real kimi-code CLI + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# Pin the CLI for reproducibility; bump alongside docs/harness/kimi.md. The scoped
# `@moonshot-ai/kimi-code` is the official package (bin `kimi`); the unscoped
# `kimi-code` on npm is NOT it.
RUN npm install -g @moonshot-ai/kimi-code@0.23.5
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
# Pre-create the config dir so the backend's dir-based detect passes (the CLI would
# create it on first run anyway); the two products share the `kimi` binary, so the
# `~/.kimi-code` dir — not the binary name — is what marks this as the current one.
mkdir -p "$KIMI_CODE_HOME"

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

host_fixture setup --agent kimi

if ! grep -q 'ez-fixture'   "$MCP";    then echo "FAIL: our mcp server not registered"; cat "$MCP"; exit 1; fi
if ! grep -q 'their-server' "$MCP";    then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'      "$MCP";    then echo "FAIL: seeded mcp key clobbered on install"; exit 1; fi
if ! grep -q 'host_fixture self-heal'   "$CONFIG"; then echo "FAIL: SessionStart hook not written"; cat "$CONFIG"; exit 1; fi
if ! grep -q 'host_fixture check-restart' "$CONFIG"; then echo "FAIL: UserPromptSubmit hook not written"; exit 1; fi
if ! grep -q 'their-session-hook' "$CONFIG"; then echo "FAIL: seeded colliding hook clobbered on install"; exit 1; fi
if ! grep -q 'their-stop-hook'    "$CONFIG"; then echo "FAIL: seeded untouched hook clobbered on install"; exit 1; fi
if ! grep -q 'kimi-k2'            "$CONFIG"; then echo "FAIL: seeded config key clobbered on install"; exit 1; fi
if ! grep -q "user's own kimi config" "$CONFIG"; then echo "FAIL: seeded comment dropped on install"; exit 1; fi

# Native second opinions (best-effort; the auth-free config parse above is authoritative).
if out=$(kimi doctor 2>&1); then
  echo "native kimi doctor ran (config validated without auth)"
else
  echo "note: kimi doctor unavailable; relied on config-file assert"
fi
if out=$(kimi mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native kimi mcp list confirmed ez-fixture"
  else
    echo "note: kimi mcp list ran but did not surface ez-fixture (likely needs a session); relied on config-file assert"
  fi
else
  echo "note: kimi mcp list unavailable (likely needs interactive /login); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$MCP"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'their-server' "$MCP"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if grep -q 'host_fixture self-heal'   "$CONFIG"; then echo "FAIL: our SessionStart hook survived uninstall"; exit 1; fi
if grep -q 'host_fixture check-restart' "$CONFIG"; then echo "FAIL: our UserPromptSubmit hook survived uninstall"; exit 1; fi
if ! grep -q 'their-session-hook' "$CONFIG"; then echo "FAIL: uninstall removed the seeded SessionStart hook"; exit 1; fi
if ! grep -q 'their-stop-hook'    "$CONFIG"; then echo "FAIL: uninstall removed the seeded Stop hook"; exit 1; fi
if ! grep -q 'kimi-k2'            "$CONFIG"; then echo "FAIL: uninstall removed the seeded config key"; exit 1; fi

echo "kimi docker leg: OK"
TEST
chmod +x /usr/local/bin/kimi-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/kimi-leg.sh"]
