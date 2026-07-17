# syntax=docker/dockerfile:1
# droid (Factory) docker leg: the real droid CLI ingesting our written config. Built +
# run by tests/docker/run.sh droid (context = repo root). The config-file parse is the
# auth-free source of truth (agentgear writes `~/.factory/mcp.json` directly, exactly the
# shape droid loads); `droid` needs a FACTORY_API_KEY for anything model-facing and has
# no documented auth-free `mcp list`, so there is no native second opinion here. The CLI
# install is tolerant: a flaky `curl | sh` never fails the leg, the file asserts do.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real droid CLI + the fixture binary; assert the lifecycle end-to-end.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Best-effort CLI install; the config-file assert below is authoritative and auth-free,
# so an install failure (URL drift, network, auth gate) must not fail the build.
RUN curl -fsSL https://app.factory.ai/cli | sh \
    || echo "note: droid CLI install failed; the leg relies on the config-file assert"
# The install script drops the binary in one of these; put them on PATH for `droid`.
ENV PATH="/usr/local/bin:/root/.local/bin:/root/.factory/bin:${PATH}"

COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/droid-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/droid-home
BASE="$HOME/.factory"
MCP="$BASE/mcp.json"
HOOKS="$BASE/hooks.json"
COMMAND="$BASE/commands/ez-fixture-plugin-hello.md"
DROID="$BASE/droids/ez-fixture-plugin-ez-helper.md"
SKILL_DIR="$BASE/skills/ez-skill"
SKILL="$SKILL_DIR/SKILL.md"
mkdir -p "$BASE"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall,
# and pre-create the config dir so detect() passes even without the CLI on PATH.
cat > "$MCP" <<'JSON'
{ "telemetry": false, "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent droid

if ! grep -q 'ez-fixture' "$MCP";   then echo "FAIL: our mcp server not registered"; cat "$MCP"; exit 1; fi
if ! grep -q 'theirs'     "$MCP";   then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q 'telemetry'  "$MCP";   then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if ! grep -q 'SessionStart' "$HOOKS"; then echo "FAIL: hooks not translated"; cat "$HOOKS"; exit 1; fi
if ! grep -q 'UserPromptSubmit' "$HOOKS"; then echo "FAIL: UserPromptSubmit hook not translated"; exit 1; fi
if [ ! -f "$COMMAND" ]; then echo "FAIL: translated command markdown missing"; exit 1; fi
if [ ! -f "$DROID" ];   then echo "FAIL: translated custom droid markdown missing"; exit 1; fi
if ! grep -q 'name: ez-fixture-plugin-ez-helper' "$DROID"; then echo "FAIL: custom droid name not namespaced"; exit 1; fi
if [ ! -f "$SKILL" ]; then echo "FAIL: translated skill SKILL.md missing"; exit 1; fi
if ! grep -q 'x-agentgear' "$SKILL"; then echo "FAIL: skill ownership tag missing"; exit 1; fi

# No native second opinion: `droid` gates on FACTORY_API_KEY for model-facing work and
# documents no auth-free `mcp list`, so the config-file assert above is authoritative.
if command -v droid >/dev/null 2>&1; then
  echo "note: droid CLI present; skipping a native check (no documented auth-free mcp list)"
else
  echo "note: droid CLI not installed; relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$MCP"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$MCP"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -f "$COMMAND" ]; then echo "FAIL: command file survived uninstall"; exit 1; fi
if [ -f "$DROID" ];   then echo "FAIL: custom droid file survived uninstall"; exit 1; fi
if [ -d "$SKILL_DIR" ]; then echo "FAIL: skill dir survived uninstall"; exit 1; fi

echo "droid docker leg: OK"
TEST
chmod +x /usr/local/bin/droid-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/droid-leg.sh"]
