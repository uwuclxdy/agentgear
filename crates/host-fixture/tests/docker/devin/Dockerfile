# syntax=docker/dockerfile:1
# devin docker leg: the real Devin CLI ingesting our written config. Built + run by
# tests/docker/run.sh devin (context = repo root). Config-file parse is the auth-free
# source of truth (agentgear writes `~/.config/devin/config.json` directly, exactly
# how `devin mcp add` would); `devin mcp list` is a best-effort native second opinion
# (auth-free per the brief, but its exact behavior headless is unverified). The CLI
# install is tolerant: a flaky `curl | bash` never fails the leg, the file asserts do.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real Devin CLI + the fixture binary; assert the lifecycle end-to-end.
# debian bookworm satisfies devin's glibc >= 2.28 / glibcxx >= 3.4.25 floor.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Best-effort CLI install; the config-file assert below is authoritative and auth-free,
# so an install failure (URL drift, network) must not fail the build.
RUN curl -fsSL https://cli.devin.ai/install.sh | bash \
    || echo "note: devin CLI install failed; the leg relies on the config-file assert"
# The install script drops the binary in one of these; put them on PATH for `devin`.
ENV PATH="/usr/local/bin:/root/.local/bin:/root/.devin/bin:${PATH}"

COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/devin-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/devin-home
BASE="$HOME/.config/devin"
CONFIG="$BASE/config.json"
SKILL="$BASE/skills/ez-fixture-plugin-hello/SKILL.md"
AGENT="$BASE/agents/ez-fixture-plugin-ez-helper/AGENT.md"
# The plugin's own skills/ IR is a distinct surface: bare `<name>/` in the .agents/skills
# scan root, never colliding with the commands-as-skills dir under $BASE/skills.
PLUGIN_SKILL_DIR="$HOME/.agents/skills/ez-skill"
PLUGIN_SKILL="$PLUGIN_SKILL_DIR/SKILL.md"
mkdir -p "$BASE"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall,
# and pre-create the config dir so detect() passes even without the CLI on PATH.
cat > "$CONFIG" <<'JSON'
{ "theme": "dark", "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent devin

if ! grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our mcp server not registered"; cat "$CONFIG"; exit 1; fi
if ! grep -q 'theirs'     "$CONFIG"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'    "$CONFIG"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if ! grep -q 'SessionStart' "$CONFIG"; then echo "FAIL: hooks not translated"; exit 1; fi
if [ ! -f "$SKILL" ];     then echo "FAIL: translated skill SKILL.md missing"; exit 1; fi
if [ ! -f "$AGENT" ];     then echo "FAIL: translated subagent AGENT.md missing"; exit 1; fi
if [ ! -f "$PLUGIN_SKILL" ]; then echo "FAIL: plugin skill SKILL.md missing"; exit 1; fi
if ! grep -q 'x-agentgear' "$PLUGIN_SKILL"; then echo "FAIL: plugin skill ownership tag missing"; exit 1; fi

# Native second opinion (best-effort; the auth-free config parse above is authoritative).
# devin 3000.6.2 rewrites config.json on `mcp list` (drops every mcpServers entry,
# adds version: 1), so snapshot the file around the native call: the file asserts
# below run on what agentgear wrote, never on the CLI's rewrite. OBSERVED 2026-08-26.
if command -v devin >/dev/null 2>&1; then
  cp "$CONFIG" "$CONFIG.pre-native"
  if out=$(devin mcp list 2>&1); then
    if echo "$out" | grep -q 'ez-fixture'; then
      echo "native devin mcp list confirmed ez-fixture"
    else
      echo "FAIL: devin mcp list ran but did not list ez-fixture"; echo "$out"; exit 1
    fi
  else
    echo "note: devin mcp list unavailable (likely needs auth); relied on config-file assert"
  fi
  mv "$CONFIG.pre-native" "$CONFIG"
else
  echo "note: devin CLI not installed; relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$CONFIG"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -d "$BASE/skills/ez-fixture-plugin-hello" ]; then echo "FAIL: skill dir survived uninstall"; exit 1; fi
if [ -d "$BASE/agents/ez-fixture-plugin-ez-helper" ]; then echo "FAIL: agent dir survived uninstall"; exit 1; fi
if [ -d "$PLUGIN_SKILL_DIR" ]; then echo "FAIL: plugin skill dir survived uninstall"; exit 1; fi

echo "devin docker leg: OK"
TEST
chmod +x /usr/local/bin/devin-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/devin-leg.sh"]
