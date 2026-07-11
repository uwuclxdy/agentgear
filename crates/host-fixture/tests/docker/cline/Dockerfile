# syntax=docker/dockerfile:1
# cline docker leg. cline is a VS Code extension with no verified auth-free headless
# CLI, so the config-file parse is the authoritative, auth-free source of truth
# (the extension reads the file on next activation); the standalone `cline` npm CLI
# is installed best-effort for a native second opinion only. Built + run by
# tests/docker/run.sh cline (context = repo root).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: Node (for the optional `cline` CLI) + the fixture binary; assert the
# lifecycle end-to-end against the files cline actually ingests.
FROM node:22-slim
# Best-effort: the CLI may be unavailable or gate on `cline auth`; the config-file
# assert below is authoritative, so a failed install must not fail the leg.
RUN npm install -g cline || echo "note: cline CLI unavailable; relying on config-file assert"
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/cline-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/cline-home
SETTINGS="$HOME/.cline/data/settings/cline_mcp_settings.json"
WORKFLOW="$HOME/Documents/Cline/Workflows/ez-fixture-plugin-hello.md"
HOOK="$HOME/Documents/Cline/Rules/Hooks/UserPromptSubmit"

# Seed the modern unified store with a foreign mcp server + an unrelated key that
# must survive install/uninstall. Its presence also makes detect() pass without the
# CLI (the `~/.cline` store dir exists).
mkdir -p "$(dirname "$SETTINGS")"
cat > "$SETTINGS" <<'JSON'
{ "someUserSetting": true, "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent cline

if ! grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our mcp server not registered"; cat "$SETTINGS"; exit 1; fi
if ! grep -q 'theirs'          "$SETTINGS"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q 'someUserSetting' "$SETTINGS"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$WORKFLOW" ];                     then echo "FAIL: translated workflow markdown missing"; exit 1; fi
if [ ! -f "$HOOK" ];                         then echo "FAIL: translated UserPromptSubmit hook missing"; exit 1; fi
if ! grep -q 'agentgear-managed:ez-fixture-plugin' "$HOOK"; then echo "FAIL: hook missing ownership tag"; exit 1; fi

# Confirm the written JSON actually parses (node is present in this stage).
node -e 'JSON.parse(require("fs").readFileSync(process.env.SETTINGS,"utf8"))' \
  || { echo "FAIL: cline_mcp_settings.json does not parse"; exit 1; }

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if out=$(cline config mcp --json 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native cline config mcp confirmed ez-fixture"
  else
    echo "note: cline config mcp ran but did not list ez-fixture (likely a separate CLI store); relied on config-file assert"
  fi
else
  echo "note: cline config mcp unavailable (likely needs auth); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$SETTINGS"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -f "$WORKFLOW" ];               then echo "FAIL: workflow survived uninstall"; exit 1; fi
if [ -f "$HOOK" ];                   then echo "FAIL: hook survived uninstall"; exit 1; fi

echo "cline docker leg: OK"
TEST
chmod +x /usr/local/bin/cline-leg.sh
SH

ENV SETTINGS=/tmp/cline-home/.cline/data/settings/cline_mcp_settings.json
ENTRYPOINT ["/usr/local/bin/cline-leg.sh"]
