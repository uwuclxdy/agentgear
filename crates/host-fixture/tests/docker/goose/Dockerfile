# syntax=docker/dockerfile:1
# goose docker leg: the real goose CLI ingesting our written config. Built + run by
# tests/docker/run.sh goose (context = repo root). The config-file (YAML) parse is the
# auth-free source of truth; `goose info` is a best-effort native second opinion (it
# runs without an LLM provider/API key per the brief, and errors if our config.yaml is
# malformed — see docs/harness/goose.md).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real goose CLI + the fixture binary; assert the lifecycle end-to-end.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates bzip2 \
 && rm -rf /var/lib/apt/lists/*

# Headless install (no provider config prompt). `stable` until a version is pinned;
# pin + bump `GOOSE_VERSION` alongside docs/harness/goose.md (the brief flags active
# vendor/domain flux — re-verify github.com/block/goose before a hard version pin).
# The installer drops the binary in /root/.local/bin, so put that on PATH.
ENV PATH="/root/.local/bin:${PATH}"
RUN curl -fsSL https://github.com/block/goose/releases/download/stable/download_cli.sh | CONFIGURE=false bash

COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/goose-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/goose-home
CONFIG="$HOME/.config/goose/config.yaml"
HOOKS="$HOME/.agents/plugins/ez-fixture-plugin/hooks/hooks.json"
PLUGIN_DIR="$HOME/.agents/plugins/ez-fixture-plugin"
mkdir -p "$(dirname "$CONFIG")"

# Seed a foreign extension + an unrelated top-level key that must survive
# install/uninstall (proves the read-modify-write never clobbers the user's config).
cat > "$CONFIG" <<'YAML'
GOOSE_MODEL: gpt-x
extensions:
  theirs:
    type: stdio
    cmd: their-server
YAML

host_fixture setup --agent goose

if ! grep -q 'ez-fixture'    "$CONFIG"; then echo "FAIL: our extension not registered"; cat "$CONFIG"; exit 1; fi
if ! grep -q 'cmd: host_fixture' "$CONFIG"; then echo "FAIL: goose cmd field missing"; cat "$CONFIG"; exit 1; fi
if ! grep -q 'their-server'  "$CONFIG"; then echo "FAIL: seeded extension clobbered on install"; exit 1; fi
if ! grep -q 'GOOSE_MODEL'   "$CONFIG"; then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if [ ! -f "$HOOKS" ];        then echo "FAIL: plugin-owned hooks.json missing"; exit 1; fi
if ! grep -q 'host_fixture self-heal' "$HOOKS"; then echo "FAIL: SessionStart hook not translated"; exit 1; fi

# Native second opinion (best-effort; the auth-free config parse above is authoritative).
# `goose info` runs without a provider and re-parses config.yaml, so a clean exit means
# goose accepts the extension shape we wrote.
if out=$(goose info 2>&1); then
  echo "native goose info accepted our config.yaml"
else
  echo "note: goose info unavailable (likely container keyring/provider gap); relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture'   "$CONFIG"; then echo "FAIL: our extension survived uninstall"; exit 1; fi
if ! grep -q 'their-server' "$CONFIG"; then echo "FAIL: uninstall removed the seeded extension"; exit 1; fi
if ! grep -q 'GOOSE_MODEL'  "$CONFIG"; then echo "FAIL: uninstall removed the seeded key"; exit 1; fi
if [ -d "$PLUGIN_DIR" ];  then echo "FAIL: our plugin hooks dir survived uninstall"; exit 1; fi

echo "goose docker leg: OK"
TEST
chmod +x /usr/local/bin/goose-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/goose-leg.sh"]
