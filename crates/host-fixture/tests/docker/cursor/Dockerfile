# syntax=docker/dockerfile:1
# cursor docker leg: cursor is GUI-first with no reliable auth-free headless CLI,
# so the file-config parse is the authoritative assert (the brief's recommended
# zero-binary path). `cursor-agent mcp list` is attempted best-effort only — it
# reads the same mcp.json but gates on auth, so it never fails the leg. Built + run
# by tests/docker/run.sh cursor (context = repo root).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: the fixture binary + a throwaway HOME; assert the lifecycle by parsing
# the written cursor config. `curl` is only for the optional cursor-agent probe.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/cursor-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/cursor-home
CURSOR="$HOME/.cursor"
MCP="$CURSOR/mcp.json"
HOOKS="$CURSOR/hooks.json"
CMD="$CURSOR/commands/ez-fixture-plugin-hello.md"
AGENT="$CURSOR/agents/ez-fixture-plugin-ez-helper.md"
mkdir -p "$CURSOR"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$MCP" <<'JSON'
{ "editorTelemetry": false, "mcpServers": { "theirs": { "type": "stdio", "command": "their-server", "args": [] } } }
JSON

# Seed a foreign hook under an event we also write to (sessionStart, collides with
# our SessionStart mapping) plus one under an event we never touch. hooks.json is
# a file separate from mcp.json, so this seed is load-bearing for never-clobber.
cat > "$HOOKS" <<'JSON'
{ "version": 1, "hooks": { "sessionStart": [ { "command": "their-session-hook" } ], "stop": [ { "command": "their-stop-hook" } ] } }
JSON

host_fixture setup --agent cursor

if ! grep -q 'ez-fixture'        "$MCP";   then echo "FAIL: our mcp server not registered";      cat "$MCP"; exit 1; fi
if ! grep -q '"type": "stdio"'   "$MCP";   then echo "FAIL: Typed shape (type:stdio) missing";   cat "$MCP"; exit 1; fi
if ! grep -q 'theirs'            "$MCP";   then echo "FAIL: seeded server clobbered on install";  exit 1; fi
if ! grep -q 'editorTelemetry'   "$MCP";   then echo "FAIL: seeded key clobbered on install";     exit 1; fi
if ! grep -q 'beforeSubmitPrompt' "$HOOKS"; then echo "FAIL: UserPromptSubmit not mapped";         cat "$HOOKS"; exit 1; fi
if ! grep -q 'their-session-hook' "$HOOKS"; then echo "FAIL: seeded sessionStart hook clobbered";  cat "$HOOKS"; exit 1; fi
if ! grep -q 'their-stop-hook'    "$HOOKS"; then echo "FAIL: seeded stop hook clobbered";          cat "$HOOKS"; exit 1; fi
if [ ! -f "$CMD" ];                        then echo "FAIL: translated command file missing";     exit 1; fi
if [ ! -f "$AGENT" ];                      then echo "FAIL: translated agent file missing";       exit 1; fi

# A second setup must be a true NoOp — which also proves the written config
# re-parses cleanly (json_edit errors on an unparseable mcp.json).
if [ "$(host_fixture setup --agent cursor)" != "NoOp" ]; then echo "FAIL: second setup was not NoOp"; exit 1; fi

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if curl -fsSL https://cursor.com/install | bash >/dev/null 2>&1; then
  export PATH="$HOME/.local/bin:$PATH"
  if out=$(cursor-agent mcp list 2>&1) && echo "$out" | grep -q 'ez-fixture'; then
    echo "native cursor-agent mcp list confirmed ez-fixture"
  else
    echo "note: cursor-agent mcp list unavailable (likely needs auth); relied on config-file assert"
  fi
else
  echo "note: cursor-agent install unavailable; relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture'      "$MCP"; then echo "FAIL: our server survived uninstall";        exit 1; fi
if ! grep -q 'theirs'       "$MCP"; then echo "FAIL: uninstall removed the seeded server";   exit 1; fi
if ! grep -q 'editorTelemetry' "$MCP"; then echo "FAIL: uninstall removed the seeded key";   exit 1; fi
if ! grep -q 'their-session-hook' "$HOOKS"; then echo "FAIL: uninstall removed the seeded sessionStart hook"; exit 1; fi
if ! grep -q 'their-stop-hook'    "$HOOKS"; then echo "FAIL: uninstall removed the seeded stop hook";         exit 1; fi
if [ -f "$CMD" ];                   then echo "FAIL: command file survived uninstall";        exit 1; fi
if [ -f "$AGENT" ];                 then echo "FAIL: agent file survived uninstall";          exit 1; fi

echo "cursor docker leg: OK"
TEST
chmod +x /usr/local/bin/cursor-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/cursor-leg.sh"]
