# syntax=docker/dockerfile:1
# openclaw docker leg: the real openclaw CLI ingesting our written config. Built +
# run by tests/docker/run.sh openclaw (context = repo root). The config-file parse is
# the auth-free source of truth; `openclaw mcp list` is a best-effort native second
# opinion (documented auth-free/local per the brief — see docs/harness/openclaw.md).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real openclaw CLI + the fixture binary; assert the lifecycle end-to-end.
FROM node:20-slim
# The install script (headless: `--no-onboard`, no ClawHub login) lands the CLI at
# build time as root; the leg adds its likely bin dirs to PATH. Tracks the script's
# latest; bump the pin alongside docs/harness/openclaw.md if the script grows one.
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL https://openclaw.ai/install.sh | bash -s -- --no-onboard
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/openclaw-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/openclaw-home
# Cover wherever the install script dropped the binary (build-time root home,
# /usr/local/bin, or the state dir) so the native second opinion can run.
export PATH="/root/.local/bin:/root/.openclaw/bin:$HOME/.openclaw/bin:/usr/local/bin:$PATH"
CONFIG="$HOME/.openclaw/openclaw.json"
mkdir -p "$HOME/.openclaw"

# Seed a foreign mcp server + an unrelated top-level key that must survive install/uninstall.
cat > "$CONFIG" <<'JSON'
{
  "theme": "dark",
  "mcp": { "servers": { "theirs": { "command": "their-server", "args": [], "env": {} } } }
}
JSON

host_fixture setup --agent openclaw

if ! grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our mcp server not registered"; cat "$CONFIG"; exit 1; fi
if ! grep -q 'theirs'     "$CONFIG"; then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q '"theme"'    "$CONFIG"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi

# Native second opinion (best-effort; auth-free config parse above is authoritative).
if command -v openclaw >/dev/null 2>&1 && out=$(openclaw mcp list 2>&1); then
  if echo "$out" | grep -q 'ez-fixture'; then
    echo "native openclaw mcp list confirmed ez-fixture"
  else
    echo "FAIL: openclaw mcp list ran but did not list ez-fixture"; echo "$out"; exit 1
  fi
else
  echo "note: openclaw mcp list unavailable; relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$CONFIG"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$CONFIG"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if ! grep -q '"theme"'  "$CONFIG"; then echo "FAIL: uninstall removed the seeded key"; exit 1; fi

echo "openclaw docker leg: OK"
TEST
chmod +x /usr/local/bin/openclaw-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/openclaw-leg.sh"]
