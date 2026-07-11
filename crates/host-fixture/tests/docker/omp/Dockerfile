# syntax=docker/dockerfile:1
# omp (oh-my-pi) docker leg: the real omp CLI ingesting our written config. omp has
# no documented auth-free headless `mcp list`/validate subcommand (MCP writes are
# validated only through the interactive `/mcp` TUI — see docs/harness/omp.md), so
# the file-config parse is the authoritative assert. Installing the real `omp` and
# running `omp --version` is a best-effort "the CLI is present + starts" second
# opinion only; it never fails the leg. Built + run by tests/docker/run.sh omp
# (context = repo root).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: the fixture binary + a throwaway HOME; assert the lifecycle by parsing the
# written omp config. `curl`/`unzip` are only for the optional real-omp install probe.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl unzip \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/omp-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/omp-home
BASE="$HOME/.omp/agent"
MCP="$BASE/mcp.json"
CMD="$BASE/commands/ez-fixture-plugin-hello.md"
AGENT="$BASE/agents/ez-fixture-plugin-ez-helper.md"
mkdir -p "$BASE"

# Seed a foreign mcp server + an unrelated top-level key ($schema, a real omp key)
# that must survive install/uninstall.
cat > "$MCP" <<'JSON'
{ "$schema": "https://x/mcp-schema.json", "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

host_fixture setup --agent omp

if ! grep -q 'ez-fixture'    "$MCP"; then echo "FAIL: our mcp server not registered";      cat "$MCP"; exit 1; fi
if ! grep -q 'host_fixture'  "$MCP"; then echo "FAIL: our mcp command missing";            cat "$MCP"; exit 1; fi
if ! grep -q 'theirs'        "$MCP"; then echo "FAIL: seeded server clobbered on install";  exit 1; fi
if ! grep -q 'schema'        "$MCP"; then echo "FAIL: seeded top-level key clobbered on install"; exit 1; fi
if [ ! -f "$CMD" ];                  then echo "FAIL: translated command file missing";     exit 1; fi
if [ ! -f "$AGENT" ];                then echo "FAIL: translated agent file missing";       exit 1; fi
if ! grep -q 'name: ez-fixture-plugin-ez-helper' "$AGENT"; then echo "FAIL: agent not re-emitted with a namespaced name"; cat "$AGENT"; exit 1; fi

# A second setup must be a true NoOp — which also proves the written config
# re-parses cleanly (json_edit errors on an unparseable mcp.json).
if [ "$(host_fixture setup --agent omp)" != "NoOp" ]; then echo "FAIL: second setup was not NoOp"; exit 1; fi

# Best-effort: install the real omp + prove the binary starts. There is no auth-free
# `mcp list` to confirm ingestion, so this never fails the leg (config parse above is
# authoritative).
if curl -fsSL https://omp.sh/install | sh >/dev/null 2>&1; then
  export PATH="$HOME/.local/bin:$HOME/.bun/bin:$PATH"
  if out=$(omp --version 2>&1); then
    echo "real omp present: $out (config-file parse is authoritative; no auth-free mcp list)"
  else
    echo "note: omp installed but --version failed; relied on config-file assert"
  fi
else
  echo "note: omp install unavailable; relied on config-file assert"
fi

host_fixture uninstall

if grep -q 'ez-fixture' "$MCP"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$MCP"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if ! grep -q 'schema'   "$MCP"; then echo "FAIL: uninstall removed the seeded top-level key"; exit 1; fi
if [ -f "$CMD" ];               then echo "FAIL: command file survived uninstall"; exit 1; fi
if [ -f "$AGENT" ];             then echo "FAIL: agent file survived uninstall"; exit 1; fi

echo "omp docker leg: OK"
TEST
chmod +x /usr/local/bin/omp-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/omp-leg.sh"]
