# syntax=docker/dockerfile:1
# gemini docker leg: the real gemini-cli ingesting our written config. Built +
# run by tests/docker/run.sh gemini (context = repo root). Config-file parse is the
# auth-free source of truth; `gemini mcp list` is a best-effort native second
# opinion (it connects locally, no model call, but may gate on auth).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real gemini-cli + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# Pin the CLI for reproducibility; bump alongside docs/harness/gemini.md.
RUN npm install -g @google/gemini-cli@0.50.0
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/gemini-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/gemini-home
SETTINGS="$HOME/.gemini/settings.json"
CMD_DIR="$HOME/.gemini/commands/ez-fixture-plugin"
mkdir -p "$HOME/.gemini"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$SETTINGS" <<'JSON'
{ "theme": "dark", "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent gemini

if ! grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our mcp server not registered"; cat "$SETTINGS"; exit 1; fi
if ! grep -q 'theirs'     "$SETTINGS"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'    "$SETTINGS"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$CMD_DIR/hello.toml" ];     then echo "FAIL: translated command TOML missing"; exit 1; fi

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if out=$(gemini mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native gemini mcp list confirmed ez-fixture"
  else
    echo "FAIL: gemini mcp list ran but did not list ez-fixture"; echo "$out"; exit 1
  fi
else
  echo "note: gemini mcp list unavailable (likely needs auth); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$SETTINGS"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -d "$CMD_DIR" ];                then echo "FAIL: command dir survived uninstall"; exit 1; fi

echo "gemini docker leg: OK"
TEST
chmod +x /usr/local/bin/gemini-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/gemini-leg.sh"]
