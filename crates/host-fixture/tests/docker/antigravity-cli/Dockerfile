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
# jq is here for the statusLine asserts only: this base has no json parser at all
# (bookworm-slim ships perl-base but not JSON::PP), and a slot value must be parsed,
# never grepped. Smallest real parser available — jq's closure downloads 0.4 MB (1.1 MB
# on disk) against python3-minimal at 2.9 MB and perl-modules at 2.7 MB.
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates bash jq \
    && rm -rf /var/lib/apt/lists/*

# Auth-free install (Go binary, no npm). The install location isn't pinned by any
# doc, so locate the dropped `agy` and put it on a standard PATH dir.
RUN bash <<'SH'
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
# Both files live in agy's global customization root; `antigravity-cli/` is the
# CLI's own settings dir and is scanned for neither.
MCP="$HOME/.gemini/config/mcp_config.json"
HOOKS="$HOME/.gemini/config/hooks.json"
# The status-line slot is the one surface that does NOT live in that customization
# root: it goes in the CLI's own settings profile, user scope only.
SETTINGS="$HOME/.gemini/antigravity-cli/settings.json"
# Passed as argv, not env, so neither `host_fixture` nor `agy` inherits a variable this
# harness invented. `${AGENTGEAR_CLIENT}` in the host declaration expands to the id.
STATUSLINE_CMD="host_fixture statusline --client antigravity-cli"
# Pre-create the CLI-specific dir so the backend's detect() passes deterministically
# (agy-on-PATH is the other signal; this keeps the leg independent of PATH quirks).
mkdir -p "$HOME/.gemini/antigravity-cli" "$HOME/.gemini/config"

# A key agy owns in its own settings file, seeded by hand. agy DOES rewrite this file in
# a real session (`antigravity_cli.rs` records 1.1.6 normalizing it and dropping unknown
# keys), but neither auth-free command this leg can reach touches it: md5 came back
# unchanged across `--version` and `plugin list` on the two versions run so far, 1.1.3
# and 1.1.7. So there is no copilot-style two-writer coexistence to prove here. What the
# seed does prove is that our whole-file rewrite (confedit::json_edit) merges beside a
# foreign key in a file the CLI owns instead of replacing it.
cat > "$SETTINGS" <<'JSON'
{ "theirSetting": "keep me" }
JSON

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
if [ ! -f "$HOOKS" ];              then echo "FAIL: hooks.json missing from the customization root"; exit 1; fi
if ! grep -q 'ez-fixture-plugin' "$HOOKS"; then echo "FAIL: our plugin hook key missing"; cat "$HOOKS"; exit 1; fi
if ! grep -q 'PreInvocation'     "$HOOKS"; then echo "FAIL: UserPromptSubmit did not land on PreInvocation"; cat "$HOOKS"; exit 1; fi
# Only agy's five legal events may be written; a dead name reads as wired and never
# fires. The validator has no teeth on hooks, so this grep is the gate.
for dead in SessionStart BeforeAgent SessionEnd Notification; do
  if grep -q "$dead" "$HOOKS"; then echo "FAIL: dead hook event $dead written"; cat "$HOOKS"; exit 1; fi
done

# Native second opinion (best-effort; auth-free per the brief, lists plugins not
# mcp servers, so it can only prove the CLI parses its config tree without erroring).
if out=$(agy plugin list 2>&1); then
  echo "agy plugin list ran (config tree parsed): $out"
else
  echo "note: agy plugin list unavailable (likely needs auth); relied on config-file assert"
fi

# FILE assert, not native — nothing auth-free makes agy render its status line, so this
# proves only that the slot landed in the right shape at the right path, not that agy
# displays it. Deliberately ordered AFTER the two agy calls above: they leave this file
# untouched today, so the day one of them starts normalizing settings.json and drops the
# key, it reds right here. That red arrives on an unrelated rebuild rather than with a
# reviewable bump, because this image installs agy unpinned (see the install above).
# Parsed rather than grepped: the value has to be at the ROOT of this file, and a
# substring check cannot tell a root key from a nested one.
#
# A fresh slot must NOT carry `enabled`: the shape copies that key off whatever it
# displaces and never synthesizes one, and an invented `enabled: true` would be a guess
# about a field whose render-time meaning is unproven. The carry itself is exercised in
# the displaced-line pass at the end of this script.
jq -e --arg cmd "$STATUSLINE_CMD" '
  (.statusLine | type) == "object"
  and .statusLine.type == "command"
  and .statusLine.command == $cmd
  and .statusLine.padding == 0
  and (.statusLine | has("enabled") | not)
  and .theirSetting == "keep me"
' "$SETTINGS" >/dev/null || { echo "FAIL: statusLine after setup is wrong"; cat "$SETTINGS"; exit 1; }

host_fixture uninstall

if grep -q 'ez-fixture' "$MCP";           then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$MCP";           then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if grep -q 'ez-fixture-plugin' "$HOOKS";  then echo "FAIL: our hook key survived uninstall"; exit 1; fi

# Nothing was stashed (no status line preceded ours), so `remove` deletes the key
# outright. `has` rather than a null check: a key left sitting at null is a value agy
# would still read, not a removal.
jq -e '(has("statusLine") | not) and .theirSetting == "keep me"' "$SETTINGS" >/dev/null \
    || { echo "FAIL: statusLine after uninstall is wrong"; cat "$SETTINGS"; exit 1; }

# Second pass, displacing a status line the user already had. `enabled` is the one key
# this shape carries verbatim off the value it replaces: it persists their own
# `/statusline off`, so dropping it would turn their bar back on showing OUR row.
cat > "$SETTINGS" <<'JSON'
{ "theirSetting": "keep me", "statusLine": { "type": "command", "command": "their-line", "enabled": false } }
JSON

host_fixture setup --agent antigravity-cli >/dev/null

jq -e --arg cmd "$STATUSLINE_CMD" '
  .statusLine.type == "command"
  and .statusLine.command == $cmd
  and .statusLine.padding == 0
  and .statusLine.enabled == false
' "$SETTINGS" >/dev/null || { echo "FAIL: enabled was not carried off the displaced line"; cat "$SETTINGS"; exit 1; }

host_fixture uninstall >/dev/null

# Whole-value equality, so our own padding is caught if it were left behind on theirs.
jq -e '.statusLine == { "type": "command", "command": "their-line", "enabled": false }' "$SETTINGS" >/dev/null \
    || { echo "FAIL: the displaced status line was not restored verbatim"; cat "$SETTINGS"; exit 1; }

echo "antigravity-cli docker leg: OK"
TEST
chmod +x /usr/local/bin/antigravity-cli-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/antigravity-cli-leg.sh"]
