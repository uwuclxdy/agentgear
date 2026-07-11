# syntax=docker/dockerfile:1
# amp docker leg: the real amp CLI ingesting our written config. Built + run by
# tests/docker/run.sh amp (context = repo root). The config-file parse is the
# auth-free source of truth; `amp mcp list` is a best-effort native second opinion
# (a login-free `amp mcp list` is reported but unconfirmed against primary docs —
# see docs/harness/amp.md), so a native mismatch is a note, not a failure.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real amp CLI + the fixture binary; assert the lifecycle end-to-end.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# Auth-free single-file (Bun-compiled) install; no login needed to install, only
# to run threads. Pin the installer URL; the script self-selects the latest build.
RUN curl -fsSL https://ampcode.com/install.sh | bash
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/amp-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/amp-home
# The build-time installer ran as root, so the binary sits under /root/.local/bin;
# add both it and the runtime home's bin so `amp` resolves regardless of target.
export PATH="$HOME/.local/bin:/root/.local/bin:$PATH"
SETTINGS="$HOME/.config/amp/settings.json"
mkdir -p "$HOME/.config/amp"

# Seed a foreign mcp server (under amp's flat key) + an unrelated top-level key
# that must survive install/uninstall.
cat > "$SETTINGS" <<'JSON'
{ "theme": "dark", "amp.mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent amp

if ! grep -q 'ez-fixture'     "$SETTINGS"; then echo "FAIL: our mcp server not registered"; cat "$SETTINGS"; exit 1; fi
if ! grep -q 'amp.mcpServers' "$SETTINGS"; then echo "FAIL: flat amp.mcpServers key missing"; exit 1; fi
if ! grep -q 'theirs'         "$SETTINGS"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'        "$SETTINGS"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if out=$(amp mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native amp mcp list confirmed ez-fixture"
  else
    echo "note: amp mcp list ran but did not list ez-fixture (may need thread auth); relied on config-file assert"
  fi
else
  echo "note: amp mcp list unavailable (likely needs login); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$SETTINGS"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$SETTINGS"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if ! grep -q '"theme"'  "$SETTINGS"; then echo "FAIL: uninstall removed the seeded key"; exit 1; fi

echo "amp docker leg: OK"
TEST
chmod +x /usr/local/bin/amp-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/amp-leg.sh"]
