#!/usr/bin/env bash
# What the image installed must be what the pin line names. The npm legs COPY this
# helper and their own Dockerfile into the image and run it as the entrypoint's last
# assert, so nothing can install after the compare: a second install of the package
# in a spelling the bump planner's literal scans cannot see (`npm i -g`, a tarball
# fetch) would otherwise leave the leg green on a version the pin does not name. The
# package arrives as two adjacent args split at the scope slash, so the leg never
# spells it whole on a line the bump planner would refuse to move.
# The version read needs only node, the legs' load-bearing runtime: the global root
# is derived from node's own prefix and needs no npm binary (measured 2026-11-07:
# a node:20-slim base served dangling npm symlinks). Residual: this reads the package
# directory's package.json, not the resolved binary; a same-bin alias install
# shadowing the binary is caught only where a leg asserts the binary's own version
# (copilot-cli).
set -euo pipefail # any failure must abort the leg, never leave it green

die() { # FAIL: <message> on stderr, exit 1
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

[ "$#" -eq 3 ] || die "usage: installed-vs-pin.sh <dockerfile-copy> <pkg-part1> <pkg-part2>"
dockerfile=$1
pkg=$2$3
[ -f "$dockerfile" ] || die "the leg's Dockerfile copy is missing: $dockerfile"

# The pin line, read exactly the way pin-staleness.sh reads it: the package's one
# `npm install -g <pkg>@<version>` line, counted over every line.
needle="npm install -g $pkg@"
hits=$(grep -nF -- "$needle" "$dockerfile") || hits=
count=$(printf '%s' "$hits" | grep -c . || true)
[ "$count" -eq 1 ] || die "$dockerfile: expected exactly one '$needle<version>' line, found $count"
line_no=${hits%%:*}
line=${hits#*:}
# everything after the needle up to the first whitespace, the same read
# pin_from_npm_line makes, so the two readers cannot disagree
pinned=${line#*"$needle"}
pinned=${pinned%%[[:space:]]*}
# a dotted version: starts with a digit and holds a dot
case $pinned in
    [0123456789]*.*) ;;
    *) die "$dockerfile:$line_no: the token after '$needle' on the pin line is not a version: '$pinned'" ;;
esac

# What the image actually runs, read back from the installed package. The global
# root comes from node's own prefix, not `npm root -g` (see header).
npm_root=$(node -p 'require("path").join(process.execPath, "..", "..", "lib", "node_modules")') || die "cannot derive the npm global root from node"
pkg_json="$npm_root/$pkg/package.json"
installed=$(node -e '
const v = require(process.argv[1]).version;
if (typeof v !== "string" || v === "") process.exit(1);
process.stdout.write(v);
' "$pkg_json") || die "cannot read the installed version from $pkg_json"

[ "$installed" = "$pinned" ] || die "installed $pkg $installed != pin $pinned: the image runs one version while the pin line in $dockerfile names the other (a second install the bump planner cannot see is the suspected cause)"
