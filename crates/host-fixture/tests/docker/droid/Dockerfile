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
# jq is here for the statusLine asserts only: this base has no json parser at all
# (bookworm-slim ships perl-base but not JSON::PP), and a slot value must be parsed,
# never grepped. Smallest real parser available — jq's closure downloads 0.4 MB (1.1 MB
# on disk) against python3-minimal at 2.9 MB and perl-modules at 2.7 MB.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl jq \
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
# The status-line slot lives in droid's own settings file, beside (not inside) the
# mcp.json every other surface here writes.
SETTINGS="$BASE/settings.json"
COMMAND="$BASE/commands/ez-fixture-plugin-hello.md"
DROID="$BASE/droids/ez-fixture-plugin-ez-helper.md"
SKILL_DIR="$BASE/skills/ez-skill"
SKILL="$SKILL_DIR/SKILL.md"
# Passed as argv, not env, so `host_fixture` never inherits a variable this harness
# invented. `${AGENTGEAR_CLIENT}` in the host declaration expands to the backend id.
STATUSLINE_CMD="host_fixture statusline --client droid"
mkdir -p "$BASE"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall,
# and pre-create the config dir so detect() passes even without the CLI on PATH.
cat > "$MCP" <<'JSON'
{ "telemetry": false, "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

# A key droid owns in its own settings file, seeded by hand: nothing auth-free makes
# the CLI write this file (checked by md5 across `droid --version` in this image), so
# there is no copilot-style two-writer coexistence to prove here. What it does prove is
# that our whole-file rewrite (confedit::json_edit) merges beside a foreign key instead
# of replacing the file.
cat > "$SETTINGS" <<'JSON'
{ "theirSetting": "keep me" }
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

# Run the real CLI BEFORE the slot assert, so the assert covers a droid that starts
# writing settings.json as well as our own write. Auth-free and file-inert today (rc 0,
# and a seeded settings.json comes back md5-identical), which is exactly the property
# that would silently stop holding. Best-effort: the CLI install above is deliberately
# tolerant, so a missing binary must not fail the leg.
if out=$(droid --version 2>&1); then
  echo "droid --version ok: $out"
else
  echo "note: droid --version unavailable; relied on config-file assert"
fi

# FILE assert, not native — droid gates model-facing work on FACTORY_API_KEY and
# nothing auth-free renders a status line, so this proves only that the slot landed in
# the right shape at the right path, not that droid displays it. Parsed rather than
# grepped: a value at the wrong nesting depth passes any substring check, and the
# `ez-fixture` greps above are blind to this key anyway (our command string says
# `host_fixture`).
#
# maxRows is asserted like type/command: `compose` structurally emits two rows whenever
# the user already had a status line, and droid is read to cap at 1 by default, so an
# unset cap would clip off exactly the row that design exists to preserve. 3 is the
# ceiling of its zod schema. Both of those come from the schema and the renderer clamp,
# never from an observed render, so what this pins is the value agentgear writes, not a
# proven display outcome (`droid.rs` STATUSLINE_MAX_ROWS carries the same caveat).
jq -e --arg cmd "$STATUSLINE_CMD" '
  (.statusLine | type) == "object"
  and .statusLine.type == "command"
  and .statusLine.command == $cmd
  and .statusLine.padding == 0
  and .statusLine.maxRows == 3
  and .theirSetting == "keep me"
' "$SETTINGS" >/dev/null || { echo "FAIL: statusLine after setup is wrong"; cat "$SETTINGS"; exit 1; }

# No native second opinion on the config itself: `droid` gates on FACTORY_API_KEY for
# model-facing work and documents no auth-free `mcp list`, so the file asserts above are
# authoritative and the version call is only a liveness + file-inertness check.

host_fixture uninstall

if grep -q 'ez-fixture' "$MCP"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$MCP"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -f "$COMMAND" ]; then echo "FAIL: command file survived uninstall"; exit 1; fi
if [ -f "$DROID" ];   then echo "FAIL: custom droid file survived uninstall"; exit 1; fi
if [ -d "$SKILL_DIR" ]; then echo "FAIL: skill dir survived uninstall"; exit 1; fi

# Nothing was stashed (the container never had a pre-existing status line), so `remove`
# deletes the key outright. `has` rather than a null check: a key left sitting at null
# is a value droid would still read, not a removal.
jq -e '(has("statusLine") | not) and .theirSetting == "keep me"' "$SETTINGS" >/dev/null \
    || { echo "FAIL: statusLine after uninstall is wrong"; cat "$SETTINGS"; exit 1; }

echo "droid docker leg: OK"
TEST
chmod +x /usr/local/bin/droid-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/droid-leg.sh"]
