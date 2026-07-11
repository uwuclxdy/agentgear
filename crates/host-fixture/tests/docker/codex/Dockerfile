# syntax=docker/dockerfile:1
# codex docker leg: the real codex CLI ingesting our written config. Built + run by
# tests/docker/run.sh codex (context = repo root). The config-file parse is the
# auth-free source of truth; `codex mcp list` is a best-effort native second opinion
# (documented local, no-API-key, but first-run auth is unverified in the brief — see
# docs/harness/codex.md).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real codex CLI + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# `@latest` until a version is verified against a live install; pin + bump it
# alongside docs/harness/codex.md (the brief could not confirm a version).
RUN npm install -g @openai/codex@latest
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/codex-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/codex-home
export CODEX_HOME="$HOME/.codex"
CONFIG="$CODEX_HOME/config.toml"
HOOKS="$CODEX_HOME/hooks.json"
PROMPT="$CODEX_HOME/prompts/ez-fixture-plugin-hello.md"
AGENT="$CODEX_HOME/agents/ez-fixture-plugin-ez-helper.toml"
mkdir -p "$CODEX_HOME"

# Seed a foreign mcp server + an unrelated top-level key + a comment that must
# survive install/uninstall (comment/key-order preservation is the toml_edit point).
cat > "$CONFIG" <<'TOML'
# the user's own codex config
model = "gpt-5.4"

[mcp_servers.theirs]
command = "their-server"
args = ["--flag"]
TOML

host_fixture setup --agent codex

if ! grep -q 'mcp_servers.ez-fixture' "$CONFIG"; then echo "FAIL: our mcp server not registered"; cat "$CONFIG"; exit 1; fi
if ! grep -q 'their-server'  "$CONFIG"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q 'gpt-5.4'       "$CONFIG"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if ! grep -q "user's own codex config" "$CONFIG"; then echo "FAIL: seeded comment dropped on install"; exit 1; fi
if [ ! -f "$PROMPT" ];       then echo "FAIL: translated prompt markdown missing"; exit 1; fi
if [ ! -f "$AGENT" ];        then echo "FAIL: translated agent toml missing"; exit 1; fi
if ! grep -q 'developer_instructions' "$AGENT"; then echo "FAIL: agent not translated to a codex subagent"; exit 1; fi
if [ ! -f "$HOOKS" ];        then echo "FAIL: translated hooks.json missing"; exit 1; fi

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if out=$(codex mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native codex mcp list confirmed ez-fixture"
  else
    echo "FAIL: codex mcp list ran but did not list ez-fixture"; echo "$out"; exit 1
  fi
else
  echo "note: codex mcp list unavailable (likely needs first-run auth); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'mcp_servers.ez-fixture' "$CONFIG"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'their-server' "$CONFIG"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if ! grep -q 'gpt-5.4'      "$CONFIG"; then echo "FAIL: uninstall removed the seeded key"; exit 1; fi
if [ -f "$PROMPT" ];        then echo "FAIL: prompt file survived uninstall"; exit 1; fi
if [ -f "$AGENT" ];         then echo "FAIL: agent file survived uninstall"; exit 1; fi

echo "codex docker leg: OK"
TEST
chmod +x /usr/local/bin/codex-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/codex-leg.sh"]
