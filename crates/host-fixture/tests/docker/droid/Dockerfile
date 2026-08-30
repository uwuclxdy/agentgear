# syntax=docker/dockerfile:1
# droid (Factory) docker leg: the real droid CLI ingesting our written config. Built +
# run by tests/docker/run.sh droid (context = repo root). The config-file parse is the
# auth-free source of truth (agentgear writes `~/.factory/mcp.json` directly, exactly the
# shape droid loads); `droid` needs a FACTORY_API_KEY for anything model-facing and has
# no documented auth-free `mcp list`, so there is no native second opinion here. The CLI
# is version-pinned and the build refuses to ship an image running anything else.

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real droid CLI + the fixture binary; assert the lifecycle end-to-end.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# pin = the droid CLI version this leg's asserts were last checked against; bump it
# alongside docs/harness/droid.md. The vendor installer
# (https://app.factory.ai/cli) hardcodes its own version and honours no env or argument
# override, so there is nothing to pin THROUGH it: this fetches the versioned artifact the
# script would have fetched, from the same URL, with the same sha256 sibling. Unpinned, an
# apt-layer invalidation moves the CLI mid-session with no reviewable change to this file
# (0.174.0 -> 0.180.0 happened exactly that way).
RUN bash <<'SH'
# -e, no fallback: an image that could not install the pinned CLI must fail the build
# rather than run the asserts against whatever else it happened to get.
set -euo pipefail
version=0.180.0

case "$(uname -m)" in
    x86_64|amd64) arch=x64 ;;
    arm64|aarch64) arch=arm64 ;;
    *) echo "droid pin: no published artifact for $(uname -m)" >&2; exit 1 ;;
esac
# The vendor ships a separate build for x64 CPUs without AVX2 and selects it by this same
# grep; mirror it so the pinned URL resolves to an artifact this machine can execute.
if [ "$arch" = x64 ] && ! grep -qi avx2 /proc/cpuinfo 2>/dev/null; then
    arch=x64-baseline
fi

base="https://downloads.factory.ai/factory-cli/releases/$version/linux/$arch"
# -f, so a 404 is an error here instead of an html page saved as the binary.
curl -fsSL -o /tmp/droid "$base/droid" \
    || { echo "droid pin: nothing published at $base/droid; is '$version' still served for $arch?" >&2; exit 1; }
# The expected hash is FETCHED from the vendor rather than written in this file, so it
# catches a corrupted or truncated download but NOT a re-upload of different bytes under
# the same version tag: that moves the artifact and its sibling hash together, and
# `droid --version` would still answer 0.180.0. The asymmetry with the agy leg (which
# hardcodes its sha512) is deliberate, and only exists because a literal url pin left that
# one nothing to fetch. Ceiling: this pins VERSION, not content. Upgrade path, if a
# same-version re-upload ever shows up: replace the fetch with the three hardcoded linux
# hashes, keyed by the $arch selected above (x64, x64-baseline, arm64 are all published).
want="$(curl -fsSL "$base/droid.sha256")" \
    || { echo "droid pin: no checksum at $base/droid.sha256" >&2; exit 1; }
echo "$want  /tmp/droid" | sha256sum -c --status \
    || { echo "droid pin: sha256 mismatch against $base/droid.sha256" >&2; exit 1; }
install -m 0755 /tmp/droid /usr/local/bin/droid
rm -f /tmp/droid

# The pin is only worth the line it is written on if the binary agrees with it.
got="$(droid --version)"
if [ "$got" != "$version" ]; then
    echo "droid pin drift: binary reports '$got', this Dockerfile pins '$version'" >&2
    exit 1
fi
SH

COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture

RUN <<'SH'
cat > /usr/local/bin/droid-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/droid-home
BASE="$HOME/.factory"
MCP="$BASE/mcp.json"
HOOKS="$BASE/hooks.json"
# droid's own settings file, beside (not inside) the mcp.json every other surface here
# writes. Nothing this backend writes lives in it anymore (the automatic status-line
# wiring is retired), so the whole file must survive every lifecycle op byte-identical.
SETTINGS="$BASE/settings.json"
COMMAND="$BASE/commands/ez-fixture-plugin-hello.md"
DROID="$BASE/droids/ez-fixture-plugin-ez-helper.md"
SKILL_DIR="$BASE/skills/ez-skill"
SKILL="$SKILL_DIR/SKILL.md"
mkdir -p "$BASE"

# Seed a foreign mcp server + an unrelated key that must survive install/uninstall,
# and pre-create the config dir so detect() passes even without the CLI on PATH.
cat > "$MCP" <<'JSON'
{ "telemetry": false, "mcpServers": { "theirs": { "command": "their-server", "args": [] } } }
JSON

# A key droid owns in its own settings file plus a user status line, seeded by hand:
# nothing auth-free makes the CLI write this file (checked by md5 across `droid
# --version` in this image), so there is no copilot-style two-writer coexistence to
# prove here. The seeded statusLine is the retired-slot contract: setup and uninstall
# must both leave the whole file byte-identical.
cat > "$SETTINGS" <<'JSON'
{ "theirSetting": "keep me", "statusLine": { "type": "command", "command": "their-line", "padding": 2 } }
JSON
cp "$SETTINGS" /tmp/settings-seed

host_fixture setup --agent droid

if ! grep -q 'ez-fixture' "$MCP";   then echo "FAIL: our mcp server not registered"; cat "$MCP"; exit 1; fi
if ! grep -q 'theirs'     "$MCP";   then echo "FAIL: seeded server clobbered on install"; exit 1; fi
if ! grep -q 'telemetry'  "$MCP";   then echo "FAIL: seeded key clobbered on install"; exit 1; fi
if ! grep -q 'SessionStart' "$HOOKS"; then echo "FAIL: hooks not translated"; cat "$HOOKS"; exit 1; fi
if ! grep -q 'UserPromptSubmit' "$HOOKS"; then echo "FAIL: UserPromptSubmit hook not translated"; exit 1; fi
if [ ! -f "$COMMAND" ]; then echo "FAIL: translated command markdown missing"; exit 1; fi
if [ ! -f "$DROID" ];   then echo "FAIL: translated custom droid markdown missing"; exit 1; fi
if ! grep -q 'name: ez-fixture-plugin-ez-helper' "$DROID"; then echo "FAIL: custom droid name not namespaced"; exit 1; fi
if [ ! -f "$SKILL" ]; then echo "FAIL: translated skill SKILL.md missing"; exit 1; fi
if ! grep -q 'x-agentgear' "$SKILL"; then echo "FAIL: skill ownership tag missing"; exit 1; fi

# Run the real CLI before the byte-identity assert, so the assert covers a droid that
# starts writing settings.json as well as our own write. Auth-free and file-inert today
# (rc 0, and a seeded settings.json comes back md5-identical), which is exactly the
# property that would silently stop holding. Hard failure rather than best-effort: the
# build pins this binary and already made it print its version, so a failure here is the
# CLI breaking under the leg's own HOME, which is a result worth reding on.
if ! out=$(droid --version 2>&1); then
  echo "FAIL: droid --version errored under the leg HOME: $out"; exit 1
fi
echo "droid --version ok: $out"

# Retired-slot contract: the seeded settings file, user statusLine included, must be
# byte-identical after setup — nothing this backend writes lives in it anymore.
cmp -s "$SETTINGS" /tmp/settings-seed || { echo "FAIL: settings.json changed across setup"; diff /tmp/settings-seed "$SETTINGS"; exit 1; }

# No native second opinion on the config itself: `droid` gates on FACTORY_API_KEY for
# model-facing work and documents no auth-free `mcp list`, so the file asserts above are
# authoritative and the version call is only a liveness + file-inertness check.

host_fixture uninstall

if grep -q 'ez-fixture' "$MCP"; then echo "FAIL: our server survived uninstall"; exit 1; fi
if ! grep -q 'theirs'   "$MCP"; then echo "FAIL: uninstall removed the seeded server"; exit 1; fi
if [ -f "$COMMAND" ]; then echo "FAIL: command file survived uninstall"; exit 1; fi
if [ -f "$DROID" ];   then echo "FAIL: custom droid file survived uninstall"; exit 1; fi
if [ -d "$SKILL_DIR" ]; then echo "FAIL: skill dir survived uninstall"; exit 1; fi

# Same byte-identity across uninstall.
cmp -s "$SETTINGS" /tmp/settings-seed || { echo "FAIL: settings.json changed across uninstall"; diff /tmp/settings-seed "$SETTINGS"; exit 1; }

echo "droid docker leg: OK"
TEST
chmod +x /usr/local/bin/droid-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/droid-leg.sh"]
