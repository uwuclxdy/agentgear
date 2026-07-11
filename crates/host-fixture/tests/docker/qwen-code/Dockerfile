# syntax=docker/dockerfile:1
# qwen-code docker leg: the real qwen-code CLI installed alongside our written
# config. Built + run by tests/docker/run.sh qwen-code (context = repo root).
# qwen-code exposes no headless `mcp list`/validate subcommand (only the interactive
# `/mcp` slash command), so a config-file parse is the source of truth; `qwen
# --version` is a best-effort proof the pinned CLI runs.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real qwen-code + the fixture binary; assert the lifecycle end-to-end.
# qwen-code requires Node.js >= 22.
FROM node:22-slim
# @latest until a version is pinned: qwen-code's release cadence is fast and no
# known-good tag was verified at authoring time; pin + bump alongside
# docs/harness/qwen-code.md once one is confirmed.
RUN npm install -g @qwen-code/qwen-code@latest
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/qwen-code-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/qwen-home
export QWEN_HOME="$HOME/.qwen"
SETTINGS="$QWEN_HOME/settings.json"
CMD_DIR="$QWEN_HOME/commands/ez-fixture-plugin"
AGENT="$QWEN_HOME/agents/ez-fixture-plugin-ez-helper.md"
mkdir -p "$QWEN_HOME"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$SETTINGS" <<'JSON'
{ "theme": "dark", "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent qwen-code

if ! grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our mcp server not registered"; cat "$SETTINGS"; exit 1; fi
if ! grep -q 'SessionStart' "$SETTINGS"; then echo "FAIL: our hooks not written"; exit 1; fi
if ! grep -q 'theirs'     "$SETTINGS"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'    "$SETTINGS"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$CMD_DIR/hello.md" ];       then echo "FAIL: copied command markdown missing"; exit 1; fi
if [ ! -f "$AGENT" ];                  then echo "FAIL: rendered agent markdown missing"; exit 1; fi

# Native sanity: the pinned CLI runs. No headless mcp-list exists, so the config
# parse above is authoritative; this only proves the package installed + launches.
if qwen --version >/dev/null 2>&1; then
  echo "qwen --version ok"
else
  echo "note: qwen --version unavailable; relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$SETTINGS"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -d "$CMD_DIR" ];                then echo "FAIL: command dir survived uninstall"; exit 1; fi
if [ -f "$AGENT" ];                  then echo "FAIL: agent file survived uninstall"; exit 1; fi

echo "qwen-code docker leg: OK"
TEST
chmod +x /usr/local/bin/qwen-code-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/qwen-code-leg.sh"]
