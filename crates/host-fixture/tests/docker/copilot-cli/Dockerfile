# syntax=docker/dockerfile:1
# copilot-cli docker leg: the real GitHub Copilot CLI ingesting our plugin natively.
# copilot >= 1.0.71 exposes a full `plugin` lifecycle (marketplace add / install /
# list / uninstall) that copies the CC tree VERBATIM, so agentgear orchestrates the
# CLI instead of translating config files. `plugin list` + `mcp list` are auth-free
# (verified 2026-07-18, docs/research/verify-copilot-cli.md), so both are
# authoritative asserts. Built + run by tests/docker/run.sh copilot-cli (context =
# repo root).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real copilot CLI (Node >=22) + the fixture binary; drive the native
# plugin lifecycle under a throwaway HOME/COPILOT_HOME.
FROM node:22-slim
# pin = the plugin-management floor, verified against a live install; bump it
# alongside docs/harness/copilot-cli.md.
RUN npm install -g @github/copilot@1.0.71
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/copilot-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/copilot-home
export COPILOT_HOME="$HOME/.copilot"
export XDG_DATA_HOME="$HOME/.local/share"
mkdir -p "$COPILOT_HOME" "$XDG_DATA_HOME"

# install: agentgear materializes the CC tree, `copilot plugin marketplace add`s it,
# then `copilot plugin install ez-fixture-plugin@ez-fixture`.
out=$(host_fixture setup --agent copilot-cli)
if [ "$out" != "Installed" ]; then echo "FAIL: first setup was not Installed: $out"; exit 1; fi

# native assert #1: copilot's own registry lists our plugin (proves the marketplace
# add + install landed).
if ! copilot plugin list | grep -q 'ez-fixture-plugin@ez-fixture'; then
    echo "FAIL: copilot plugin list missing our plugin"; copilot plugin list; exit 1
fi

# native assert #2 (auth-free): copilot loads the plugin's mcp server verbatim. It
# must appear under `Plugin servers` — agentgear writes no user mcp-config.json now,
# so a listed server proves NATIVE plugin ingestion, not the old translate path.
mcp=$(copilot mcp list 2>&1) || { echo "FAIL: copilot mcp list errored"; echo "$mcp"; exit 1; }
if ! echo "$mcp" | grep -q 'Plugin servers'; then echo "FAIL: no 'Plugin servers' section (native ingestion failed)"; echo "$mcp"; exit 1; fi
if ! echo "$mcp" | grep -q 'ez-fixture'; then echo "FAIL: copilot mcp list missing plugin server"; echo "$mcp"; exit 1; fi

# a second setup must be a true NoOp (present + current version).
if [ "$(host_fixture setup --agent copilot-cli)" != "NoOp" ]; then echo "FAIL: second setup was not NoOp"; exit 1; fi

host_fixture uninstall

if copilot plugin list | grep -q 'ez-fixture-plugin@ez-fixture'; then echo "FAIL: our plugin survived uninstall"; exit 1; fi
if copilot mcp list 2>&1 | grep -q 'ez-fixture'; then echo "FAIL: plugin mcp server survived uninstall"; exit 1; fi

echo "copilot-cli docker leg: OK"
TEST
chmod +x /usr/local/bin/copilot-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/copilot-leg.sh"]
