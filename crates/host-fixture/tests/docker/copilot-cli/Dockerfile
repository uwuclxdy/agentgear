# syntax=docker/dockerfile:1
# copilot-cli docker leg: the real GitHub Copilot CLI ingesting our written config.
# `copilot mcp list` is auth-free since 1.0.70 (verified 2026-07-16, see
# docs/research/verify-copilot-cli.md), so the native listing is an authoritative
# assert alongside the config-file parse. Built + run by tests/docker/run.sh
# copilot-cli (context = repo root).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real copilot CLI (Node >=22) + the fixture binary; assert the lifecycle
# by parsing the written copilot config under a throwaway COPILOT_HOME.
FROM node:22-slim
# pin = the version verified against a live install; bump it alongside
# docs/harness/copilot-cli.md.
RUN npm install -g @github/copilot@1.0.70
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/copilot-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export COPILOT_HOME=/tmp/copilot-home/.copilot
MCP="$COPILOT_HOME/mcp-config.json"
HOOKS="$COPILOT_HOME/hooks/ez-fixture-plugin.json"
USER_HOOKS="$COPILOT_HOME/hooks/user-own.json"
AGENT="$COPILOT_HOME/agents/ez-fixture-plugin-ez-helper.agent.md"
SKILL_DIR="$COPILOT_HOME/skills/ez-skill"
SKILL="$SKILL_DIR/SKILL.md"
mkdir -p "$COPILOT_HOME/hooks" "$COPILOT_HOME/agents"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall,
# plus a user hook file at a DIFFERENT name than ours (copilot loads every
# hooks/*.json) — we own only hooks/<plugin>.json, so it is load-bearing for never-clobber.
cat > "$MCP" <<'JSON'
{ "editorHint": true, "mcpServers": { "theirs": { "type": "local", "command": "their-server", "args": [] } } }
JSON
cat > "$USER_HOOKS" <<'JSON'
{ "version": 1, "hooks": { "sessionStart": [ { "type": "command", "bash": "their-hook" } ] } }
JSON

host_fixture setup --agent copilot-cli

if ! grep -q 'ez-fixture'      "$MCP";   then echo "FAIL: our mcp server not registered";    cat "$MCP"; exit 1; fi
if ! tr -d ' \n' < "$MCP" | grep -q '"tools":\["\*"\]'; then echo "FAIL: copilot tools:[\"*\"] shape missing"; cat "$MCP"; exit 1; fi
if ! grep -q 'their-server'    "$MCP";   then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q 'editorHint'      "$MCP";   then echo "FAIL: seeded key clobbered on install";    exit 1; fi
if [ ! -f "$HOOKS" ];                    then echo "FAIL: owned hooks file missing";           exit 1; fi
if ! grep -q 'userPromptSubmitted' "$HOOKS"; then echo "FAIL: UserPromptSubmit not mapped";    cat "$HOOKS"; exit 1; fi
if ! grep -q '"bash"'          "$HOOKS"; then echo "FAIL: hook shell string not under bash";   cat "$HOOKS"; exit 1; fi
if ! grep -q 'their-hook'      "$USER_HOOKS"; then echo "FAIL: seeded user hook file touched"; exit 1; fi
if [ ! -f "$AGENT" ];                    then echo "FAIL: translated agent file missing";      exit 1; fi
if [ ! -f "$SKILL" ];                    then echo "FAIL: translated skill SKILL.md missing";  exit 1; fi
if ! grep -q 'x-agentgear'     "$SKILL"; then echo "FAIL: skill ownership tag missing";        exit 1; fi

# A second setup must be a true NoOp — which also proves the written config
# re-parses cleanly (json_edit errors on an unparseable mcp-config.json).
if [ "$(host_fixture setup --agent copilot-cli)" != "NoOp" ]; then echo "FAIL: second setup was not NoOp"; exit 1; fi

# Native assert (auth-free since 1.0.70): copilot itself must parse the file and
# list our server AND the seeded user one — a shape copilot rejects voids the whole
# file, taking the user's servers down with it.
out=$(copilot mcp list 2>&1) || { echo "FAIL: copilot mcp list errored"; echo "$out"; exit 1; }
echo "$out" | grep -q 'ez-fixture' || { echo "FAIL: copilot mcp list missing ez-fixture"; echo "$out"; exit 1; }
echo "$out" | grep -q 'theirs'     || { echo "FAIL: copilot mcp list missing seeded server"; echo "$out"; exit 1; }

host_fixture uninstall

if grep -q 'ez-fixture'  "$MCP"; then echo "FAIL: our server survived uninstall";       exit 1; fi
if ! grep -q 'their-server' "$MCP"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if ! grep -q 'editorHint'   "$MCP"; then echo "FAIL: uninstall removed the seeded key";    exit 1; fi
if [ -f "$HOOKS" ];             then echo "FAIL: our hooks file survived uninstall";        exit 1; fi
if ! grep -q 'their-hook' "$USER_HOOKS"; then echo "FAIL: uninstall removed the seeded user hook file"; exit 1; fi
if [ -f "$AGENT" ];            then echo "FAIL: agent file survived uninstall";             exit 1; fi
if [ -d "$SKILL_DIR" ];        then echo "FAIL: skill dir survived uninstall";              exit 1; fi

echo "copilot-cli docker leg: OK"
TEST
chmod +x /usr/local/bin/copilot-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/copilot-leg.sh"]
