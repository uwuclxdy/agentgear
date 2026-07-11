# syntax=docker/dockerfile:1
# antigravity-cli docker leg: the real `agy` CLI ingesting our written config.
# Built + run by tests/docker/run.sh antigravity-cli (context = repo root).
# Config-file parse is the source of truth: `agy` has NO documented auth-free
# `mcp list`/`mcp validate` (only an interactive `/mcp` overlay + Google Sign-In),
# so we assert the files land where the CLI reads them and prove `agy` installed.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real antigravity-cli + the fixture binary; assert the lifecycle end-to-end.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates bash \
    && rm -rf /var/lib/apt/lists/*

# Auth-free install (Go binary, no npm). The install location isn't pinned by any
# doc, so locate the dropped `agy` and put it on a standard PATH dir.
RUN <<'SH'
set -euo pipefail
curl -fsSL https://antigravity.google/cli/install.sh | bash
agy_bin="$(command -v agy || find / -type f -name agy 2>/dev/null | head -n1)"
if [ -z "$agy_bin" ]; then echo "FAIL: agy not found after install" >&2; exit 1; fi
[ "$agy_bin" = /usr/local/bin/agy ] || ln -sf "$agy_bin" /usr/local/bin/agy
SH

COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/antigravity-cli-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/antigravity-home
MCP="$HOME/.gemini/config/mcp_config.json"
HOOKS="$HOME/.gemini/antigravity-cli/hooks.json"
# Pre-create the CLI-specific dir so the backend's detect() passes deterministically
# (agy-on-PATH is the other signal; this keeps the leg independent of PATH quirks).
mkdir -p "$HOME/.gemini/antigravity-cli" "$HOME/.gemini/config"

# Prove the real CLI installed (best-effort: some builds gate --version on nothing).
agy --version || echo "note: agy --version unavailable; continuing with config-file assert"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall.
cat > "$MCP" <<'JSON'
{ "theme": "dark", "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent antigravity-cli

if ! grep -q 'ez-fixture' "$MCP";  then echo "FAIL: our mcp server not registered"; cat "$MCP"; exit 1; fi
if ! grep -q 'theirs'     "$MCP";  then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'    "$MCP";  then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$HOOKS" ];              then echo "FAIL: hooks.json missing"; exit 1; fi
if ! grep -q 'ez-fixture-plugin' "$HOOKS"; then echo "FAIL: our plugin hook key missing"; cat "$HOOKS"; exit 1; fi

# Native second opinion (best-effort; auth-free per the brief, lists plugins not
# mcp servers, so it can only prove the CLI parses its config tree without erroring).
if out=$(agy plugin list 2>&1); then
  echo "agy plugin list ran (config tree parsed): $out"
else
  echo "note: agy plugin list unavailable (likely needs auth); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$MCP";           then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$MCP";           then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if grep -q 'ez-fixture-plugin' "$HOOKS";  then echo "FAIL: our hook key survived uninstall"; exit 1; fi

echo "antigravity-cli docker leg: OK"
TEST
chmod +x /usr/local/bin/antigravity-cli-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/antigravity-cli-leg.sh"]
