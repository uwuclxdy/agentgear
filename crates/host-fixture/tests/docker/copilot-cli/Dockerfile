# syntax=docker/dockerfile:1
# copilot-cli docker leg: the real GitHub Copilot CLI ingesting our plugin natively.
# copilot's `plugin` lifecycle (marketplace add / install / list / uninstall) loads a
# local-marketplace plugin live from its source dir (>= 1.0.81), so agentgear
# orchestrates the CLI instead of translating config files. `plugin list` + `mcp list`
# are auth-free (verified 2026-07-18), so both are authoritative asserts. Built + run
# by tests/docker/run.sh copilot-cli (context = repo root).

# Stage 1: build the fixture host (all backends) from the workspace.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p host-fixture --release

# Stage 2: real copilot CLI (Node >=22) + the fixture binary; drive the native
# plugin lifecycle under a throwaway HOME/COPILOT_HOME.
FROM node:22-slim
# pin = the version this leg runs against (the backend's own 1.0.92 floor is
# enforced in the crate).
RUN npm install -g @github/copilot@1.0.105
COPY --from=build /src/target/release/host_fixture /usr/local/bin/host_fixture
COPY crates/host-fixture/plugin /plugin-tree

RUN <<'SH'
cat > /usr/local/bin/copilot-leg.sh <<'TEST'
#!/usr/bin/env bash
set -euo pipefail

export HOME=/tmp/copilot-home
export COPILOT_HOME="$HOME/.copilot"
export XDG_DATA_HOME="$HOME/.local/share"
mkdir -p "$COPILOT_HOME" "$XDG_DATA_HOME"

# The pin must be what the binary actually reports, else the leg is green for the
# wrong version (a runtime HOME cache can outlive the npm pin).
ver=$(copilot --version) || { echo "FAIL: copilot --version errored"; exit 1; }
got=${ver%%$'\n'*}
[ "$got" = "GitHub Copilot CLI 1.0.105." ] || { echo "copilot pin drift: binary reports '$got', this Dockerfile pins '1.0.105'"; exit 1; }

# settings.json is where real `copilot plugin marketplace add`/`plugin install` write
# `extraKnownMarketplaces`/`enabledPlugins` (confirmed against a live container write,
# not assumed). The automatic status-line wiring is retired: nothing this backend
# writes lives in this file anymore, so a seeded user statusLine must survive every
# lifecycle op byte-identical — the one thing no hermetic test can reach, since
# `fake_copilot` persists its own registry to `fake-copilot-state.json` and never
# touches settings.json.
#
# A foreign marketplace (seeded below through `copilot plugin marketplace add`, under a
# name different from ours) and a seeded user `enabledPlugins` entry must survive our
# teardown: `remove` takes back only OUR marketplace and OUR plugin id.
#
# Passed as argv, not env, so neither `host_fixture` nor `copilot` inherits a variable
# this harness invented.
SETTINGS="$COPILOT_HOME/settings.json"
cat > "$SETTINGS" <<'JSON'
{ "theirSetting": "keep me", "statusLine": { "type": "command", "command": "their-line", "padding": 2 }, "enabledPlugins": { "their-plugin@their-marketplace": true } }
JSON

# A foreign marketplace, registered through copilot's own command before setup; it must
# survive the whole lifecycle untouched — remove() drops only OUR marketplace.
mkdir -p /tmp/their-marketplace/.claude-plugin
cat > /tmp/their-marketplace/.claude-plugin/marketplace.json <<'JSON'
{ "name": "their-marketplace", "description": "theirs", "owner": { "name": "someone" }, "plugins": [ { "name": "their-plugin", "source": "./", "description": "theirs" } ] }
JSON
copilot plugin marketplace add /tmp/their-marketplace

# install: agentgear materializes the CC tree, `copilot plugin marketplace add`s it,
# then `copilot plugin install ez-fixture-plugin@ez-fixture-plugin`.
out=$(host_fixture setup --agent copilot-cli)
if [ "$out" != "Installed" ]; then echo "FAIL: first setup was not Installed: $out"; exit 1; fi

# native assert #1: copilot's own registry lists our plugin (proves the marketplace
# add + install landed).
# Every copilot listing is captured before it is searched: piped straight into an
# early-exiting `grep -q` under pipefail, copilot dies of EPIPE and the pipeline's
# status flips, which reads a negative check as passed.
plugins=$(copilot plugin list) || { echo "FAIL: copilot plugin list errored"; exit 1; }
if ! grep -q 'ez-fixture-plugin@ez-fixture' <<<"$plugins"; then
    echo "FAIL: copilot plugin list missing our plugin"; echo "$plugins"; exit 1
fi

# native assert #2 (auth-free): copilot loads the plugin's mcp server verbatim. It
# must appear under `Plugin servers` — agentgear writes no user mcp-config.json now,
# so a listed server proves NATIVE plugin ingestion, not the old translate path.
mcp=$(copilot mcp list 2>&1) || { echo "FAIL: copilot mcp list errored"; echo "$mcp"; exit 1; }
if ! grep -q 'Plugin servers' <<<"$mcp"; then echo "FAIL: no 'Plugin servers' section (native ingestion failed)"; echo "$mcp"; exit 1; fi
if ! grep -q 'ez-fixture' <<<"$mcp"; then echo "FAIL: copilot mcp list missing plugin server"; echo "$mcp"; exit 1; fi

# Retired-slot contract, parsed for real rather than grepped: the user's seeded
# statusLine must still be exactly what it was after setup, beside the
# extraKnownMarketplaces/enabledPlugins keys real copilot wrote into the same file.
node -e '
const fs = require("fs");
const [settingsPath] = process.argv.slice(1);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
const slot = settings.statusLine;
const want = { type: "command", command: "their-line", padding: 2 };
if (JSON.stringify(slot) !== JSON.stringify(want)) {
  console.error("statusLine changed across setup, want", JSON.stringify(want), "got", JSON.stringify(slot));
  process.exit(1);
}
for (const key of ["extraKnownMarketplaces", "enabledPlugins"]) {
  if (!Object.hasOwn(settings, key)) {
    console.error(key, "written by copilot was lost");
    process.exit(1);
  }
}
if (!Object.hasOwn(settings, "theirSetting")) {
  console.error("the seeded theirSetting was lost");
  process.exit(1);
}
' "$SETTINGS" || { echo "FAIL: settings.json state after setup is wrong (see node error above)"; cat "$SETTINGS"; exit 1; }

# a second setup must be a true NoOp (present + current version).
if [ "$(host_fixture setup --agent copilot-cli)" != "NoOp" ]; then echo "FAIL: second setup was not NoOp"; exit 1; fi

# Instructions surface: the tagged block in copilot's user-scope
# copilot-instructions.md. The user's own line is seeded before setup and must
# survive both install and uninstall byte-identical.
INSTRUCTIONS="$COPILOT_HOME/copilot-instructions.md"
mkdir -p "$(dirname "$INSTRUCTIONS")"
printf 'user own rule\n' > "$INSTRUCTIONS"
host_fixture setup --agent copilot-cli > /dev/null
if ! grep -q 'agentgear:ez-fixture-plugin@ez-fixture-plugin begin' "$INSTRUCTIONS"; then
  echo "FAIL: instructions block missing from copilot-instructions.md"; cat "$INSTRUCTIONS"; exit 1
fi
grep -q 'user own rule' "$INSTRUCTIONS" || { echo "FAIL: user copilot-instructions.md line clobbered on install"; exit 1; }
host_fixture uninstall > /dev/null
if grep -q 'agentgear:ez-fixture-plugin' "$INSTRUCTIONS"; then
  echo "FAIL: instructions block survived uninstall"; exit 1
fi
grep -q 'user own rule' "$INSTRUCTIONS" || { echo "FAIL: user copilot-instructions.md line clobbered on uninstall"; exit 1; }
# Re-install so the leg's own uninstall step below exercises a full teardown.
host_fixture setup --agent copilot-cli > /dev/null

out=$(host_fixture uninstall)
[ "$out" = "Removed" ] || { echo "FAIL: final uninstall was not Removed: $out"; exit 1; }

plugins=$(copilot plugin list) || { echo "FAIL: copilot plugin list errored after uninstall"; exit 1; }
if grep -q 'ez-fixture-plugin@ez-fixture' <<<"$plugins"; then echo "FAIL: our plugin survived uninstall"; echo "$plugins"; exit 1; fi
mcp=$(copilot mcp list 2>&1) || { echo "FAIL: copilot mcp list errored after uninstall"; echo "$mcp"; exit 1; }
if grep -q 'ez-fixture' <<<"$mcp"; then echo "FAIL: plugin mcp server survived uninstall"; echo "$mcp"; exit 1; fi

# Our marketplace must be gone; the foreign one must survive.
markets=$(copilot plugin marketplace list) || { echo "FAIL: copilot plugin marketplace list errored"; exit 1; }
if grep -q 'ez-fixture-plugin' <<<"$markets"; then echo "FAIL: our marketplace survived uninstall"; echo "$markets"; exit 1; fi
if ! grep -q 'their-marketplace' <<<"$markets"; then echo "FAIL: foreign marketplace did not survive"; echo "$markets"; exit 1; fi

# After uninstall: our marketplace key is gone from extraKnownMarketplaces, our
# enabledPlugins key is gone, the user's foreign enabledPlugins entry survives, and the
# user's statusLine/theirSetting are byte-identical.
node -e '
const fs = require("fs");
const [settingsPath] = process.argv.slice(1);
const settings = JSON.parse(fs.readFileSync(settingsPath, "utf8"));
if (settings.extraKnownMarketplaces && Object.hasOwn(settings.extraKnownMarketplaces, "ez-fixture-plugin")) {
  console.error("our marketplace key survived uninstall");
  process.exit(1);
}
if (settings.enabledPlugins && Object.hasOwn(settings.enabledPlugins, "ez-fixture-plugin@ez-fixture-plugin")) {
  console.error("our enabledPlugins key survived uninstall");
  process.exit(1);
}
if (!(settings.enabledPlugins && settings.enabledPlugins["their-plugin@their-marketplace"] === true)) {
  console.error("the seeded foreign enabledPlugins entry did not survive");
  process.exit(1);
}
if (!Object.hasOwn(settings, "theirSetting")) {
  console.error("the seeded theirSetting was lost");
  process.exit(1);
}
const slot = settings.statusLine;
const want = { type: "command", command: "their-line", padding: 2 };
if (JSON.stringify(slot) !== JSON.stringify(want)) {
  console.error("statusLine changed across uninstall, want", JSON.stringify(want), "got", JSON.stringify(slot));
  process.exit(1);
}
' "$SETTINGS" || { echo "FAIL: settings.json state after uninstall is wrong (see node error above)"; cat "$SETTINGS"; exit 1; }

# Live pickup: a `--path` source, edited after setup, reaches copilot on self-heal by
# re-staging the pointer alone — no `plugin uninstall`/`plugin install`, and the row
# never reads `(disabled)`.
cp -a /plugin-tree /tmp/plugin-tree
out=$(host_fixture setup --agent copilot-cli --path /tmp/plugin-tree)
if [ "$out" != "Installed" ]; then echo "FAIL: --path setup was not Installed: $out"; exit 1; fi
printf -- '---\ndescription: probecmd live-load probe\n---\nsay probe\n' > /tmp/plugin-tree/commands/probecmd.md
out=$(host_fixture self-heal)
if [ "$out" != "Repaired" ]; then echo "FAIL: self-heal did not repair the tree edit: $out"; exit 1; fi
skills=$(copilot skill list) || { echo "FAIL: copilot skill list errored"; exit 1; }
if ! grep -q -w 'probecmd' <<<"$skills"; then echo "FAIL: copilot skill list missing probecmd"; echo "$skills"; exit 1; fi
plugins=$(copilot plugin list) || { echo "FAIL: copilot plugin list errored after re-stage"; exit 1; }
if ! grep -q 'ez-fixture-plugin@ez-fixture-plugin' <<<"$plugins"; then
    echo "FAIL: our plugin missing after re-stage"; echo "$plugins"; exit 1
fi
our_row=$(grep 'ez-fixture-plugin@ez-fixture-plugin' <<<"$plugins")
if ! grep -F -q '(enabled)' <<<"$our_row"; then
    echo "FAIL: our plugin is not (enabled) after a re-stage"; echo "$plugins"; exit 1
fi
if grep -F -q '(disabled)' <<<"$our_row"; then
    echo "FAIL: our plugin reads (disabled) after a re-stage"; echo "$plugins"; exit 1
fi

echo "copilot-cli docker leg: OK"
TEST
chmod +x /usr/local/bin/copilot-leg.sh
SH

ENTRYPOINT ["/usr/local/bin/copilot-leg.sh"]
