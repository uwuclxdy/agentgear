# syntax=docker/dockerfile:1
# antigravity-cli docker leg: the real `agy` CLI ingesting our written config.
# Built + run by tests/docker/run.sh antigravity-cli (context = repo root).
# `agy mcp` (1.4.8) ships headless add/remove/list/enable/disable, so the leg runs
# agy's OWN native writer (`mcp add`) against the shared mcp_config.json agentgear
# writes, then asserts our entries, the user's seeded keys and agy's own entry all
# survive it — and that uninstall still removes only ours.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real antigravity-cli + the fixture binary; assert the lifecycle end-to-end.
# jq: debian-slim ships no node/jq, and the parsed mcp_config.json asserts need one.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates bash jq \
    && rm -rf /var/lib/apt/lists/*

# pin = the agy version this leg's asserts were last checked against. Unpinned, an
# apt-layer invalidation moves the CLI mid-session with no reviewable change to this
# file (1.1.3 -> 1.1.7 happened exactly that way).
#
# The vendor installer (https://antigravity.google/cli/install.sh) resolves its download
# from a manifest endpoint serving the LATEST build only, and accepts no argument but
# --dir, so there is nothing to pin THROUGH it and this carries the resolved artifact
# instead. The GCS build-id suffix is not derivable from the version string, so the pin
# has to be the whole URL. Re-derive all three fields from
# https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/linux_amd64.json
# which returns exactly {version, url, sha512}. Skipping the script also skips its
# trailing `agy install` shell-rc handoff, which this leg never reads: the entrypoint
# runs under its own HOME.
RUN bash <<'SH'
# -e, no fallback: an image that could not install the pinned CLI must fail the build
# rather than run the asserts against whatever else it happened to get.
set -euo pipefail
version=1.4.10
url=https://storage.googleapis.com/antigravity-public/antigravity-cli/1.4.10-5501573669978112/linux-x64/cli_linux_x64.tar.gz
sha512=3a14c21697e86349ce8402e0b1889cbd1d3a5f4e2cb4dc43aae1c48c404240c2ccff9f775eec277e1f759234a33ec026645c63ed22ab6f6ebede561eda7c38b6

# linux-x64 only, by choice and not by oversight: the vendor script auto-detected every
# platform, but a second pinned arch cannot be exercised by this project or by CI (both
# are x86_64), so it would ship as an unverified claim that only ever fires on a machine
# none of us can test on. Do NOT restore auto-detection to "fix" an arch failure here:
# that silently unpins the leg again, which is the one thing this block exists to stop.
# Adding a pinned arch arm beside x86_64 is the supported way.
case "$(uname -m)" in
    x86_64|amd64) ;;
    *)
        # Assume the reader has this message and nothing else: name the endpoint, the
        # platform token shape, and the fields, since none of it is guessable. The arm64
        # artifact sits under linux-arm/, not linux-arm64/, so the url cannot be derived
        # by pattern from the x64 one -- it has to be read off the manifest.
        echo "agy pin: this leg pins the linux-x64 artifact; $(uname -m) is not pinned." >&2
        echo "  To add it, GET the manifest for your platform:" >&2
        echo "    https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/<platform>.json" >&2
        echo "  where <platform> looks like linux_arm64 / linux_amd64 / darwin_arm64." >&2
        echo "  It returns {version, url, sha512}; copy all three into a new case arm" >&2
        echo "  beside the x86_64 one above, setting the same three variables." >&2
        exit 1
        ;;
esac

# -f, so a 404 is an error here instead of an html page saved as the tarball.
curl -fsSL -o /tmp/agy.tar.gz "$url" \
    || { echo "agy pin: nothing served at $url; re-resolve the manifest and bump all three fields" >&2; exit 1; }
echo "$sha512  /tmp/agy.tar.gz" | sha512sum -c --status \
    || { echo "agy pin: sha512 mismatch, the pinned url no longer serves the pinned build" >&2; exit 1; }
tar -xzf /tmp/agy.tar.gz -C /tmp antigravity
install -m 0755 /tmp/antigravity /usr/local/bin/agy
rm -f /tmp/agy.tar.gz /tmp/antigravity

# The pin is only worth the line it is written on if the binary agrees with it.
got="$(agy --version)"
if [ "$got" != "$version" ]; then
    echo "agy pin drift: binary reports '$got', this Dockerfile pins '$version'" >&2
    exit 1
fi
SH

COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/agy-mcp-check.jq <<'JQ'
# Parsed check of ~/.gemini/config/mcp_config.json for one lifecycle phase. Emits
# one key-path per line for every missing/wrong key; empty output = all present.
def srv($n): .mcpServers[$n];

# `$post` matches `$pre` up to an empty args/env/headers the writer dropped. This is
# agentgear's own equivalence (mcpjson::matches_rendered): every key in the post-writer
# entry must exist in the pre-writer one and equal it, and a pre-writer key missing from
# post is tolerated only as an empty args/env/headers.
# jq compares numbers by value (1 == 1.0) where serde_json's Value equality does not; our entries carry no numeric field, and the self-heal NoOp step right after this identity runs the real Rust comparison.
def matches_rendered($pre; $post):
  ($post | type == "object") and ($pre | type == "object")
  and ($post | to_entries | all(.[]; .key as $k | ($pre | has($k)) and $pre[$k] == .value))
  and ($pre | keys | all(.[]; . as $k |
        ($post | has($k)) or
        ((["args","env","headers"] | index($k)) != null and ($pre[$k] == {} or $pre[$k] == []))));

def seeded_missing:
  (if (.theme == "dark") then empty else "theme" end),
  (if ((srv("theirs") | .command == "their-server") and (srv("theirs") | .args == ["keep"]) and (srv("theirs") | .env == {"K": "V"})) then empty else "mcpServers.theirs" end);

def ours_present_missing:
  (if ((srv("ez-fixture") | .command == "host_fixture") and (srv("ez-fixture") | .args == ["mcp"])) then empty else "mcpServers.ez-fixture" end),
  (if (srv("ez-fixture-sse") | .serverUrl == "http://127.0.0.1:39622/sse") then empty else "mcpServers.ez-fixture-sse" end);

def ours_identity_missing:
  (if matches_rendered($pre[0].mcpServers["ez-fixture"]; srv("ez-fixture")) then empty else "mcpServers.ez-fixture (changed by the native writer)" end),
  (if matches_rendered($pre[0].mcpServers["ez-fixture-sse"]; srv("ez-fixture-sse")) then empty else "mcpServers.ez-fixture-sse (changed by the native writer)" end);

def ours_absent_missing:
  (if (srv("ez-fixture") == null) then empty else "mcpServers.ez-fixture (uninstall left it)" end),
  (if (srv("ez-fixture-sse") == null) then empty else "mcpServers.ez-fixture-sse (uninstall left it)" end);

def writer_missing:
  (if ((srv("agy-native-server") | .command == "echo") and (srv("agy-native-server") | .args == ["hello"])) then empty else "mcpServers.agy-native-server" end);

seeded_missing,
(if $phase == "setup" then ours_present_missing
 elif $phase == "writer" then (ours_identity_missing, writer_missing)
 else (ours_absent_missing, writer_missing) end)
JQ

cat > /usr/local/bin/antigravity-cli-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/antigravity-home
# Both files live in agy's global customization root; `antigravity-cli/` is the
# CLI's own settings dir and is scanned for neither.
MCP="$HOME/.gemini/config/mcp_config.json"
HOOKS="$HOME/.gemini/config/hooks.json"
# agy's own settings profile, user scope only. Nothing this backend writes lives in it
# anymore (the automatic status-line wiring is retired), so the seeded bytes below must
# survive the whole leg byte-identical: the leg compares the file against a single seed
# copy taken right after seeding, and again after every agy or host_fixture call
# (`agy --version`, setup, `agy mcp list`, `agy mcp add`, self-heal, uninstall).
SETTINGS="$HOME/.gemini/antigravity-cli/settings.json"
# Pre-create the CLI-specific dir so the backend's detect() passes deterministically
# (agy-on-PATH is the other signal; this keeps the leg independent of PATH quirks).
mkdir -p "$HOME/.gemini/antigravity-cli" "$HOME/.gemini/config"

# A key agy owns in its own settings file plus a user status line, seeded by hand. The
# statusLine is the retired-slot contract: these exact bytes are the seed the leg
# compares the file against after every agy or host_fixture step, so a write any one of
# them makes is caught where it happens even if a later step reverts it byte-exactly.
cat > "$SETTINGS" <<'JSON'
{ "theirSetting": "keep me", "statusLine": { "type": "command", "command": "their-line", "enabled": false } }
JSON
cp "$SETTINGS" /tmp/settings-seed

settings_unchanged() {
  cmp -s "$SETTINGS" /tmp/settings-seed || { echo "FAIL: settings.json changed across $1"; diff /tmp/settings-seed "$SETTINGS"; exit 1; }
}

# Hard failure rather than best-effort: the build pins this binary and already made it
# print its version, so a failure here is the CLI breaking under the leg's own HOME.
agy --version || { echo "FAIL: agy --version errored under the leg HOME"; exit 1; }
settings_unchanged 'agy --version'

# Seed a foreign mcp server with non-empty args/env plus an unrelated root key. The
# non-empty seed keeps the survive assert non-degenerate: agy 1.4.8's writer drops
# empty args/env during re-serialization, so an empty seed would read as surviving
# through the drop rather than through the assert. Both must survive install/uninstall.
cat > "$MCP" <<'JSON'
{ "theme": "dark", "mcpServers": { "theirs": { "command": "their-server", "args": ["keep"], "env": { "K": "V" } } } }
JSON

host_fixture setup --agent antigravity-cli
settings_unchanged setup

check_mcp() {
  local phase="$1" missing
  missing="$(jq -r --arg phase "$phase" --slurpfile pre /tmp/mcp-pre-writer -f /usr/local/bin/agy-mcp-check.jq "$MCP")"
  if [ -n "$missing" ]; then
    echo "FAIL: mcp_config.json after $phase is wrong, missing/wrong keys:"
    printf '%s\n' "$missing"
    cat "$MCP"
    exit 1
  fi
}

# The writer phase's identity check reads the pre-writer copy; the setup check only
# needs the file to exist for the slurpfile.
echo '{}' > /tmp/mcp-pre-writer
check_mcp setup

if [ ! -f "$HOOKS" ];              then echo "FAIL: hooks.json missing from the customization root"; exit 1; fi
if ! grep -q 'ez-fixture-plugin' "$HOOKS"; then echo "FAIL: our plugin hook key missing"; cat "$HOOKS"; exit 1; fi
if ! jq -e '."ez-fixture-plugin".PreInvocation | any(.[]; .type == "command" and .command == "host_fixture check-restart")' "$HOOKS" >/dev/null; then
  echo "FAIL: UserPromptSubmit did not land on PreInvocation (no host_fixture check-restart under ez-fixture-plugin.PreInvocation)"; cat "$HOOKS"; exit 1
fi
# agy 1.4.8 lists six legal hook events: PreToolUse, PostToolUse, SessionStart,
# PreInvocation, PostInvocation, Stop. A CC event with no analog in that set reads as
# wired and never fires, so the dead names below stay banned; the validator has no
# teeth on hooks, so this grep is the gate.
for dead in BeforeAgent SessionEnd Notification; do
  if grep -q "$dead" "$HOOKS"; then echo "FAIL: dead hook event $dead written"; cat "$HOOKS"; exit 1; fi
done
# agentgear maps CC's UserPromptSubmit onto PreInvocation and wires no SessionStart
# hook for agy today; 1.4.8 lists SessionStart as legal, so it is not dead above.
# This assert pins current behaviour, not a harness rule.
if grep -q 'SessionStart' "$HOOKS"; then echo "FAIL: agentgear wrote a SessionStart hook for agy (unexpected)"; cat "$HOOKS"; exit 1; fi

# The real CLI ingests the config agentgear wrote: `agy mcp list` (auth-free at the
# pin) must list our stdio server by whole-token match and leave the file byte-identical.
cp "$MCP" /tmp/mcp-pre-list
list="$(agy mcp list)"
if ! printf '%s\n' "$list" | awk '$1 == "ez-fixture" { found = 1 } END { if (!found) exit 1 }'; then
  echo "FAIL: agy mcp list does not list ez-fixture"; printf '%s\n' "$list"; exit 1
fi
cmp -s "$MCP" /tmp/mcp-pre-list || { echo "FAIL: agy mcp list rewrote mcp_config.json"; diff /tmp/mcp-pre-list "$MCP"; exit 1; }
settings_unchanged 'agy mcp list'

# The harness's OWN native writer against the shared mcp_config.json agentgear wrote:
# `agy mcp add` adds a server of its own beside ours. Its failure fails the leg, and
# `! cmp -s` against the pre-writer copy fails the leg if it wrote nothing.
cp "$MCP" /tmp/mcp-pre-writer
cp "$HOOKS" /tmp/hooks-pre-writer
agy mcp add agy-native-server echo hello
if cmp -s "$MCP" /tmp/mcp-pre-writer; then
  echo "FAIL: agy mcp add wrote nothing (mcp_config.json unchanged)"
  exit 1
fi
cmp -s "$HOOKS" /tmp/hooks-pre-writer || { echo "FAIL: agy mcp add rewrote hooks.json"; diff /tmp/hooks-pre-writer "$HOOKS"; exit 1; }
settings_unchanged 'agy mcp add'

# All three mcp entries (the user's, ours, agy's own) plus the seeded root key must
# survive the native writer at their seeded paths, ours by identity up to the empty
# args/env the writer drops.
check_mcp writer

# agentgear's own probe must still read its entries as healthy after the native
# writer; NoOp also proves it rewrites nothing.
cp "$MCP" /tmp/mcp-pre-heal
cp "$HOOKS" /tmp/hooks-pre-heal
heal="$(printf '%s' '{"source":"startup"}' | host_fixture self-heal)"
if [ "$heal" != "NoOp" ]; then
  echo "FAIL: self-heal after the native writer printed '$heal', want 'NoOp': agentgear no longer reads its own entries as healthy"
  exit 1
fi
if ! cmp -s "$MCP" /tmp/mcp-pre-heal; then
  echo "FAIL: self-heal rewrote mcp_config.json"; diff /tmp/mcp-pre-heal "$MCP"; exit 1
fi
if ! cmp -s "$HOOKS" /tmp/hooks-pre-heal; then
  echo "FAIL: self-heal rewrote hooks.json"; diff /tmp/hooks-pre-heal "$HOOKS"; exit 1
fi
settings_unchanged self-heal

host_fixture uninstall
settings_unchanged uninstall

# uninstall removes only ours: agy's own entry and the user's seeded keys stay.
check_mcp uninstall

if grep -q 'ez-fixture' "$MCP"; then echo "FAIL: our server survived uninstall"; cat "$MCP"; exit 1; fi

if grep -q 'ez-fixture-plugin' "$HOOKS";  then echo "FAIL: our hook key survived uninstall"; exit 1; fi

echo "antigravity-cli docker leg: OK"
TEST
chmod +x /usr/local/bin/antigravity-cli-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/antigravity-cli-leg.sh"]
