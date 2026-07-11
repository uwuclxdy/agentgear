# syntax=docker/dockerfile:1
# kiro docker leg: the real kiro-cli ingesting our written config. Built + run by
# tests/docker/run.sh kiro (context = repo root). Config-file parse is the auth-free
# source of truth (the backend only writes JSON, needs zero kiro auth); the
# `kiro-cli` install + `kiro-cli mcp list` are a best-effort native second opinion
# (the installer's real value is at first-use/login, unconfirmed pre-login — see
# docs/harness/kiro.md).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: try the real kiro-cli + the fixture binary; assert the lifecycle end-to-end.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates bash unzip tar \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/kiro-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/kiro-home
export KIRO_HOME="$HOME/.kiro"
export PATH="$HOME/.local/bin:$KIRO_HOME/bin:$PATH"
MCP="$KIRO_HOME/settings/mcp.json"
AGENT="$KIRO_HOME/agents/default.json"
mkdir -p "$KIRO_HOME/settings" "$KIRO_HOME/agents"

# Best-effort real install: its payoff is the first-use login flow (unconfirmed
# pre-login), so a failure never fails the leg — the config-file assert below is
# auth-free by construction and detect() rides off the seeded KIRO_HOME dir.
if curl -fsSL https://cli.kiro.dev/install | bash; then
  echo "kiro-cli installed"
else
  echo "note: kiro-cli install failed; leg relies on the config-file assert"
fi

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$MCP" <<'JSON'
{ "someOtherSetting": true, "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON
# Seed a real user default agent (identity + a hook of their own) so we can prove
# our hooks merge into it without disturbing the user's definition.
cat > "$AGENT" <<'JSON'
{ "name": "default", "prompt": "you are helpful", "hooks": { "stop": [ { "command": "user-own-stop-hook" } ] } }
JSON

host_fixture setup --agent kiro

if ! grep -q 'ez-fixture'        "$MCP";   then echo "FAIL: our mcp server not registered"; cat "$MCP"; exit 1; fi
if ! grep -q 'theirs'            "$MCP";   then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q 'someOtherSetting'  "$MCP";   then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if ! grep -q 'agentSpawn'        "$AGENT"; then echo "FAIL: SessionStart not mapped into agentSpawn"; cat "$AGENT"; exit 1; fi
if ! grep -q 'userPromptSubmit'  "$AGENT"; then echo "FAIL: userPromptSubmit hook missing"; exit 1; fi
if ! grep -q 'user-own-stop-hook' "$AGENT"; then echo "FAIL: user's own hook clobbered on install"; exit 1; fi
if ! grep -q 'you are helpful'   "$AGENT"; then echo "FAIL: user's agent identity clobbered on install"; exit 1; fi

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if out=$(kiro-cli mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native kiro-cli mcp list confirmed ez-fixture"
  else
    echo "note: kiro-cli mcp list ran but did not list ez-fixture (may gate on login)"; echo "$out"
  fi
else
  echo "note: kiro-cli mcp list unavailable (likely needs login); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture'   "$MCP";   then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'     "$MCP";   then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if grep -q 'agentSpawn'   "$AGENT"; then echo "FAIL: our hook survived uninstall"; exit 1; fi
if ! grep -q 'user-own-stop-hook' "$AGENT"; then echo "FAIL: uninstall removed the user's own hook"; exit 1; fi

echo "kiro docker leg: OK"
TEST
chmod +x /usr/local/bin/kiro-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/kiro-leg.sh"]
