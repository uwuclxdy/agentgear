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

# pin = the agy version this leg's asserts were last checked against; bump it alongside
# docs/harness/antigravity-cli.md. Unpinned, an apt-layer invalidation moves the CLI
# mid-session with no reviewable change to this file (1.1.3 -> 1.1.7 happened exactly
# that way).
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
version=1.1.7
url=https://storage.googleapis.com/antigravity-public/antigravity-cli/1.1.7-5951805767680000/linux-x64/cli_linux_x64.tar.gz
sha512=720d5a7ff256aa5dd6712513cd5eb6fe031cf9e7523a33bcbda7755120ced53bb64ff985b402ce068e5895e0ffb348c2632545039a1dde6daad591f164d5852f

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
cat > /usr/local/bin/antigravity-cli-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/antigravity-home
# Both files live in agy's global customization root; `antigravity-cli/` is the
# CLI's own settings dir and is scanned for neither.
MCP="$HOME/.gemini/config/mcp_config.json"
HOOKS="$HOME/.gemini/config/hooks.json"
# agy's own settings profile, user scope only. Nothing this backend writes lives in it
# anymore (the automatic status-line wiring is retired), so the whole file must survive
# every lifecycle op byte-identical.
SETTINGS="$HOME/.gemini/antigravity-cli/settings.json"
# Pre-create the CLI-specific dir so the backend's detect() passes deterministically
# (agy-on-PATH is the other signal; this keeps the leg independent of PATH quirks).
mkdir -p "$HOME/.gemini/antigravity-cli" "$HOME/.gemini/config"

# A key agy owns in its own settings file plus a user status line, seeded by hand. agy
# DOES rewrite this file in a real session (`antigravity_cli.rs` records 1.1.6
# normalizing it and dropping unknown keys), but neither auth-free command this leg can
# reach touches it. The seeded statusLine is the retired-slot contract: setup and
# uninstall must both leave the whole file byte-identical.
cat > "$SETTINGS" <<'JSON'
{ "theirSetting": "keep me", "statusLine": { "type": "command", "command": "their-line", "enabled": false } }
JSON
cp "$SETTINGS" /tmp/settings-seed

# Hard failure rather than best-effort: the build pins this binary and already made it
# print its version, so a failure here is the CLI breaking under the leg's own HOME.
agy --version || { echo "FAIL: agy --version errored under the leg HOME"; exit 1; }

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

# Retired-slot contract: the seeded settings file, user statusLine included, must be
# byte-identical after setup — nothing this backend writes lives in it anymore.
cmp -s "$SETTINGS" /tmp/settings-seed || { echo "FAIL: settings.json changed across setup"; diff /tmp/settings-seed "$SETTINGS"; exit 1; }

host_fixture uninstall

if grep -q 'ez-fixture' "$MCP";           then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$MCP";           then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if grep -q 'ez-fixture-plugin' "$HOOKS";  then echo "FAIL: our hook key survived uninstall"; exit 1; fi

# Same byte-identity across uninstall.
cmp -s "$SETTINGS" /tmp/settings-seed || { echo "FAIL: settings.json changed across uninstall"; diff /tmp/settings-seed "$SETTINGS"; exit 1; }

echo "antigravity-cli docker leg: OK"
TEST
chmod +x /usr/local/bin/antigravity-cli-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/antigravity-cli-leg.sh"]
