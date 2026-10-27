#!/usr/bin/env bash
# Staleness alarm for the pinned docker legs. Each Dockerfile pins the CLI version
# its asserts were last checked against; this asks the vendor what it serves today
# and fails naming EVERY leg whose pin differs from it. The compare is exact on
# purpose: a minor-version allowance stays silent through every patch release inside
# the series, which is the drift this exists to catch.
#
# Every `<leg>/Dockerfile` in the docker dir must be classified exactly once: LEGS is
# checked against the leg's vendor, UNPINNED is unpinned on purpose and never parsed. A
# leg in neither list, in both, or listed without its Dockerfile fails the run, so a leg
# cannot arrive unclassified. Residual: a leg left in UNPINNED after someone pins it
# stays unwatched until it moves to LEGS.
#
# Known gap: droid's updater prefers a download-plan API when its remote config names one,
# so a staged rollout can move users ahead of the LATEST pointer this script reads and the
# job still reads ok until LATEST flips.
#
# Scheduled only (`.github/workflows/pin-staleness.yml`): a vendor outage reds here
# instead of a PR.
#
# exit codes: 0 every pin is current (or allowed); 1 a pin is stale, or an allowance
# no longer matches the gap it excuses; 2 a Dockerfile pin could not be read, or the
# docker legs are not all classified; 3 a vendor could not be reached, a local tool the
# read needs failed (no sha256sum on PATH, say), or a vendor answered in a shape this
# script does not recognise (an edited agy installer included). When several apply, 2
# wins over 3, and 3 over 1.
#
# usage: pin-staleness.sh [docker-dir]   (default: this script's own directory)
set -euo pipefail

unset CDPATH
DOCKER_DIR="${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}"
[ -d "$DOCKER_DIR" ] || { printf 'pin-staleness: no docker dir at %s\n' "$DOCKER_DIR" >&2; exit 2; }

# Checked legs: each version below is read from its Dockerfile and compared.
LEGS=(augment copilot-cli gemini kimi qwen-code crush antigravity-cli droid)
# Every other docker leg dir: unpinned on purpose, never parsed for a version.
UNPINNED=(amp cline codex cursor devin goose kilo kiro omp openclaw opencode)

# npm-installed legs: the pin is the `@<version>` on that package's one install line.
declare -A NPM_PKG=(
    [augment]=@augmentcode/auggie
    [copilot-cli]=@github/copilot
    [gemini]=@google/gemini-cli
    [kimi]=@moonshot-ai/kimi-code
    [qwen-code]=@qwen-code/qwen-code
    [crush]=@charmland/crush
)

# The one vendor whose version source this script derives from its own installer: a moved
# endpoint reds here instead of leaving a frozen manifest answering an old version forever.
AGY_INSTALLER_URL=https://antigravity.google/cli/install.sh
# sha256 of the installer's bytes exactly as served (`curl -fsSL <url> | sha256sum`).
# The derivation reads this exact text: a line-shape check proves a line exists, not that
# it is the one in use, so any edit to the installer reds until someone re-reads it. The
# Dockerfiles pin the artifacts they fetch by hash the same way.
AGY_INSTALLER_SHA256=62966c07365423bd4dc209355060744058fb30d60f5323e2d360e39de64e5042
# The manifest path the agy installer builds for the linux/amd64 runner this job uses.
AGY_MANIFEST_PATH=manifests/linux_amd64.json
# The installer line that builds that path; any other shape means the path above may be
# one the vendor no longer reads.
# shellcheck disable=SC2016 # the installer's own unexpanded text, matched literally
AGY_MANIFEST_LINE='MANIFEST_URL="$DOWNLOAD_BASE_URL/manifests/$platform.json"'
# droid's artifact host publishes the version it serves at this plain-text pointer, the one
# droid's own HTTP update check reads (0.250.0). The installer page that also names it
# (https://app.factory.ai/cli) can answer a bot challenge (HTTP 403) instead.
DROID_LATEST_URL=https://downloads.factory.ai/factory-cli/LATEST

# Per-leg allowance: leg -> '<pin>:<vendor version>' it excuses. Empty by default. It
# excuses exactly the gap observed when it was added, so the next vendor release reds
# again; one that no longer equals its leg's gap, or that survives into a run where
# the pin is current, is itself a failure.
declare -A ALLOW_STALE=()

die() {
    printf 'pin-staleness: %s\n' "$*" >&2
    exit 2
}

TMP=$(mktemp -d) || die 'cannot create a temp dir to read vendor answers'
trap 'rm -rf -- "$TMP"' EXIT
ERR=$TMP/err
BODY=$TMP/body
NESTED=$TMP/nested

one_match() { # <leg> <what> <file> <count>
    [ "$4" = 1 ] || die "$1: expected exactly one $2 in $3, found $4"
}

count_nonblank() {
    printf '%s' "$1" | grep -c . || true
}

first_line() { # <file> -> its first non-blank line, empty when it has none
    local line
    while IFS= read -r line; do
        [ -z "$line" ] || { printf '%s' "$line"; break; }
    done < "$1"
    return 0
}

stderr_suffix() { # <file> -> ': <its first line>' when it has one
    local line
    line=$(first_line "$1")
    [ -z "$line" ] || printf ': %s' "$line"
}

valid_version() {
    # A dotted numeric core with an optional -/+ suffix is the only shape a vendor serves;
    # `0..`, `1` and `0-` are junk that would otherwise read as a version and name the wrong
    # failure class.
    local core
    case "$1" in
        '' | [!0-9]* | *[!0-9A-Za-z.+-]* | *..* | *[.+-]) return 1 ;;
    esac
    core=${1%%[-+]*}
    case "$core" in
        *[!0-9.]*) return 1 ;;
    esac
    case "$core" in
        *.*) return 0 ;;
    esac
    return 1
}

join_by() {
    local sep=$1 out='' item
    shift
    for item in "$@"; do
        out+="${out:+$sep}$item"
    done
    printf '%s' "$out"
}

pin_from_npm_line() { # <leg> <file> <pkg>
    local leg=$1 file=$2 needle="npm install -g $3@" hits count line version
    [ -n "$3" ] || die "$leg: no NPM_PKG entry names its package"
    hits=$(grep -nF -- "$needle" "$file" || true)
    count=$(count_nonblank "$hits")
    one_match "$leg" "'$needle<version>' line" "$file" "$count"
    line=${hits#*:}
    version=${line#*"$needle"}
    version=${version%%[[:space:]]*}
    valid_version "$version" || die "$leg: unparseable version '$version' on the pin line in $file"
    printf '%s\n' "$version"
}

pin_from_version_line() { # <leg> <file>
    local leg=$1 file=$2 hits count line version
    hits=$(grep -nE '^version=' "$file" || true)
    count=$(count_nonblank "$hits")
    one_match "$leg" "'version=<version>' line" "$file" "$count"
    line=${hits#*:}
    version=${line#version=}
    version=${version%%[[:space:]]*}
    valid_version "$version" || die "$leg: unparseable version '$version' on the pin line in $file"
    printf '%s\n' "$version"
}

# Each current_* reads one vendor answer into CURRENT. A failure records WHY in
# VENDOR_KIND/VENDOR_DETAIL: `unreachable` is the tool itself failing, `unparseable` is
# a payload that arrived but holds no single valid version.
VENDOR_KIND=''
VENDOR_DETAIL=''
CURRENT=''

vendor_failed() { # <kind> <detail>
    VENDOR_KIND=$1
    VENDOR_DETAIL=$2
    return 1
}

current_from_npm() { # <pkg>
    local pkg=$1 rc=0
    CURRENT=$(npm view "$pkg" version 2>"$ERR") || rc=$?
    if [ "$rc" -ne 0 ]; then
        vendor_failed unreachable "npm view $pkg exited $rc$(stderr_suffix "$ERR")"
        return 1
    fi
    if ! valid_version "$CURRENT"; then
        vendor_failed unparseable "npm view $pkg served no single version$(stderr_suffix "$ERR")"
        return 1
    fi
}

current_from_json() { # <url> -> the manifest's `version` field
    local url=$1 rc=0
    # A command substitution drops NULs and trailing newlines, so jq reads the bytes off
    # disk; `read -d ''` succeeds only on reaching a NUL, a shape this reader does not
    # recognise.
    curl -fsSL --max-time 60 "$url" >"$BODY" 2>"$ERR" || rc=$?
    if [ "$rc" -ne 0 ]; then
        vendor_failed unreachable "curl exited $rc for $url$(stderr_suffix "$ERR")"
        return 1
    fi
    if IFS= read -r -d '' _ <"$BODY"; then
        vendor_failed unparseable "NUL byte in the body at $url"
        return 1
    fi
    CURRENT=$(jq -er '.version' <"$BODY" 2>"$ERR") || rc=$?
    if [ "$rc" -eq 126 ] || [ "$rc" -eq 127 ]; then
        vendor_failed unreachable "jq exited $rc on the manifest from $url$(stderr_suffix "$ERR")"
        return 1
    fi
    if [ "$rc" -ne 0 ] || ! valid_version "$CURRENT"; then
        vendor_failed unparseable "no single .version in the manifest from $url$(stderr_suffix "$ERR")"
        return 1
    fi
}

current_from_latest() { # <url> -> the one version line its body holds
    local url=$1 rc=0 lines=()
    curl -fsSL --max-time 60 "$url" >"$BODY" 2>"$ERR" || rc=$?
    if [ "$rc" -ne 0 ]; then
        vendor_failed unreachable "curl exited $rc for $url$(stderr_suffix "$ERR")"
        return 1
    fi
    # A bash string cannot hold a NUL, so mapfile would drop one and let a body that is not
    # one version line read as if it were; `read -d ''` succeeds only on reaching a NUL.
    if IFS= read -r -d '' _ <"$BODY"; then
        vendor_failed unparseable "NUL byte in the body at $url"
        return 1
    fi
    # mapfile splits on newlines without the command substitution's newline stripping, so a
    # body carrying more than one line is counted, not folded into one.
    mapfile -t lines <"$BODY"
    if [ "${#lines[@]}" -ne 1 ]; then
        vendor_failed unparseable "expected one line at $url, found ${#lines[@]}"
        return 1
    fi
    if ! valid_version "${lines[0]}"; then
        vendor_failed unparseable "unparseable version $(LC_ALL=C printf '%q' "${lines[0]}") at $url"
        return 1
    fi
    CURRENT=${lines[0]}
}

current_from_agy() { # <install.sh url> -> the version its manifest `<base>/manifests/linux_amd64.json` names
    local url=$1 rc=0 body='' bases count base got
    curl -fsSL --max-time 60 "$url" >"$BODY" 2>"$ERR" || rc=$?
    if [ "$rc" -ne 0 ]; then
        vendor_failed unreachable "curl exited $rc for $url$(stderr_suffix "$ERR")"
        return 1
    fi
    got=$(sha256sum <"$BODY" 2>"$ERR") || rc=$?
    if [ "$rc" -ne 0 ]; then
        vendor_failed unreachable "sha256sum exited $rc on the installer from $url$(stderr_suffix "$ERR")"
        return 1
    fi
    got=${got%% *}
    if [ "$got" != "$AGY_INSTALLER_SHA256" ]; then
        vendor_failed unparseable "AGY INSTALLER CHANGED: re-read $url and confirm it still sets one plain https DOWNLOAD_BASE_URL, builds the manifest url as '$AGY_MANIFEST_LINE', and gives linux x86_64 the platform in '$AGY_MANIFEST_PATH'; then set AGY_INSTALLER_SHA256=$got"
        return 1
    fi
    body=$(<"$BODY")
    bases=$(printf '%s\n' "$body" | sed -nE 's/^DOWNLOAD_BASE_URL="?([^"]*)"?[[:space:]]*$/\1/p')
    count=$(count_nonblank "$bases")
    if [ "$count" -ne 1 ]; then
        vendor_failed unparseable "expected one 'DOWNLOAD_BASE_URL=' line in the installer at $url, found $count"
        return 1
    fi
    base=$bases
    # A quoted, parameterised or non-https base is a reshaped installer, not an outage:
    # curl would reject it and read as unreachable.
    case "$base" in
        https://*[!A-Za-z0-9./:_-]* | https://) base='' ;;
        https://*) ;;
        *) base='' ;;
    esac
    [ -n "$base" ] || { vendor_failed unparseable "DOWNLOAD_BASE_URL '$bases' in the installer at $url is not a plain https url"; return 1; }
    count=$(printf '%s\n' "$body" | grep -cxF -- "$AGY_MANIFEST_LINE" || true)
    if [ "$count" -ne 1 ]; then
        vendor_failed unparseable "expected one '$AGY_MANIFEST_LINE' line in the installer at $url, found $count"
        return 1
    fi
    current_from_json "$base/$AGY_MANIFEST_PATH"
}

stale=()
allowed=()
unreadable=()
drifted_allowances=()
fetch_failed=()
unparseable=()
unclassified=()
missing=()
# leg -> the status its row ended at before the compare, empty once the allowance was tested.
declare -A UNEVALUATED=()

# A leg in neither list could carry a pin nobody watches, so every `<leg>/Dockerfile`
# must sit in exactly one of them. Names are compared, never the Dockerfile's text.
declare -A SEEN=() IN_LEGS=() IN_UNPINNED=()
for leg in "${LEGS[@]}"; do IN_LEGS[$leg]=1; done
for leg in "${UNPINNED[@]}"; do IN_UNPINNED[$leg]=1; done
# run.sh builds `<harness>/Dockerfile` for any name, a dot-dir or a nested path included.
shopt -s dotglob
for f in "$DOCKER_DIR"/*/Dockerfile; do
    [ -f "$f" ] || continue
    leg=${f%/Dockerfile}
    leg=${leg##*/}
    SEEN[$leg]=1
    in_legs=${IN_LEGS[$leg]:-}
    in_unpinned=${IN_UNPINNED[$leg]:-}
    if [ -n "$in_legs" ] && [ -n "$in_unpinned" ]; then
        unclassified+=("$leg: it is in both LEGS and UNPINNED, keep it in one")
    elif [ -z "$in_legs$in_unpinned" ]; then
        unclassified+=("$leg: add it to LEGS with a version source, or to UNPINNED")
    fi
done
shopt -u dotglob
# -L: run.sh follows a symlinked dir in `<harness>`, so the sweep does too.
find_rc=0
find -L "$DOCKER_DIR" -mindepth 3 -name Dockerfile -print0 >"$NESTED" 2>"$ERR" || find_rc=$?
[ "$find_rc" -eq 0 ] || unclassified+=("find exited $find_rc sweeping $DOCKER_DIR for nested Dockerfiles$(stderr_suffix "$ERR")")
while IFS= read -r -d '' f; do
    unclassified+=("${f#"$DOCKER_DIR"/}: a Dockerfile below a leg dir, which neither list can name")
done <"$NESTED"

# A listed leg whose dir is gone was renamed or deleted: its pin is unwatched until the
# list says so.
declare -A LISTED=()
for leg in "${LEGS[@]}" "${UNPINNED[@]}"; do
    [ -z "${LISTED[$leg]:-}" ] || continue
    LISTED[$leg]=1
    [ -z "${SEEN[$leg]:-}" ] || continue
    if [ -n "${IN_LEGS[$leg]:-}" ]; then where=LEGS; else where=UNPINNED; fi
    missing+=("$leg: listed in $where but has no Dockerfile")
done

# The rows and the counts come from this one list: a name listed twice is checked and
# counted once, and a leg in both lists is already an unclassified failure, left unchecked.
CHECKED=()
declare -A CHECK_SEEN=()
for leg in "${LEGS[@]}"; do
    [ -z "${CHECK_SEEN[$leg]:-}" ] || continue
    CHECK_SEEN[$leg]=1
    [ -z "${IN_UNPINNED[$leg]:-}" ] || continue
    CHECKED+=("$leg")
done

printf 'pin-staleness: checking %d pinned docker legs in %s\n' "${#CHECKED[@]}" "$DOCKER_DIR"
printf '%-16s %-12s %-12s %s\n' leg pin current status
for leg in "${CHECKED[@]}"; do
    file="$DOCKER_DIR/$leg/Dockerfile"
    if [ ! -f "$file" ]; then
        printf '%-16s %-12s %-12s %s\n' "$leg" '-' '-' 'MISSING DOCKERFILE'
        UNEVALUATED[$leg]='MISSING DOCKERFILE'
        continue
    fi
    pin=''
    pin_rc=0
    case "$leg" in
        augment | copilot-cli | gemini | kimi | qwen-code | crush)
            pin=$(pin_from_npm_line "$leg" "$file" "${NPM_PKG[$leg]:-}" 2>"$ERR") || pin_rc=$?
            ;;
        antigravity-cli | droid)
            pin=$(pin_from_version_line "$leg" "$file" 2>"$ERR") || pin_rc=$?
            ;;
        *)
            printf 'pin-staleness: %s: no version source configured\n' "$leg" >"$ERR"
            pin_rc=1
            ;;
    esac
    if [ "$pin_rc" -ne 0 ]; then
        pin_why=$(first_line "$ERR" | sed 's/^pin-staleness: //')
        printf '%-16s %-12s %-12s %s\n' "$leg" '-' '-' "PIN UNREADABLE ($pin_why)"
        unreadable+=("$pin_why")
        UNEVALUATED[$leg]='PIN UNREADABLE'
        continue
    fi

    CURRENT=''
    VENDOR_KIND=''
    VENDOR_DETAIL=''
    vendor_rc=0
    case "$leg" in
        augment | copilot-cli | gemini | kimi | qwen-code | crush)
            current_from_npm "${NPM_PKG[$leg]}" || vendor_rc=$?
            ;;
        antigravity-cli)
            current_from_agy "$AGY_INSTALLER_URL" || vendor_rc=$?
            ;;
        droid)
            current_from_latest "$DROID_LATEST_URL" || vendor_rc=$?
            ;;
    esac
    if [ "$vendor_rc" -ne 0 ]; then
        if [ "$VENDOR_KIND" = unparseable ]; then
            printf '%-16s %-12s %-12s %s\n' "$leg" "$pin" '-' "PAYLOAD UNRECOGNISED ($VENDOR_DETAIL)"
            unparseable+=("$leg ($VENDOR_DETAIL)")
            UNEVALUATED[$leg]='PAYLOAD UNRECOGNISED'
        else
            printf '%-16s %-12s %-12s %s\n' "$leg" "$pin" '-' "FETCH FAILED ($VENDOR_DETAIL)"
            fetch_failed+=("$leg ($VENDOR_DETAIL)")
            UNEVALUATED[$leg]='FETCH FAILED'
        fi
        continue
    fi

    allow="${ALLOW_STALE[$leg]:-}"
    if [ "$CURRENT" = "$pin" ]; then
        if [ -n "$allow" ]; then
            printf '%-16s %-12s %-12s %s\n' "$leg" "$pin" "$CURRENT" "ALLOWANCE UNUSED ('$allow')"
            drifted_allowances+=("$leg: allowance '$allow' but the pin is current")
        else
            printf '%-16s %-12s %-12s %s\n' "$leg" "$pin" "$CURRENT" ok
        fi
        continue
    fi

    if [ -z "$allow" ]; then
        printf '%-16s %-12s %-12s %s\n' "$leg" "$pin" "$CURRENT" STALE
        stale+=("$leg ($pin != $CURRENT)")
    elif [ "$allow" = "$pin:$CURRENT" ]; then
        printf '%-16s %-12s %-12s %s\n' "$leg" "$pin" "$CURRENT" 'ALLOWED (stale by design)'
        allowed+=("$leg")
    else
        printf '%-16s %-12s %-12s %s\n' "$leg" "$pin" "$CURRENT" "ALLOWANCE DRIFTED ('$allow')"
        drifted_allowances+=("$leg: allowance '$allow' != '$pin:$CURRENT'")
    fi
done

# An allowance excuses one leg's gap, so a leg whose row ended before the compare (no
# Dockerfile, an unreadable pin, a vendor failure) never tested it: flag it as unevaluated
# rather than leave it silently unspent. A leg in both lists is never checked at all.
if [ "${#ALLOW_STALE[@]}" -gt 0 ]; then
    for leg in "${!ALLOW_STALE[@]}"; do
        if [ -z "${CHECK_SEEN[$leg]:-}" ] || [ -n "${IN_UNPINNED[$leg]:-}" ]; then
            drifted_allowances+=("$leg: allowance for a leg this job does not check")
        elif [ -n "${UNEVALUATED[$leg]:-}" ]; then
            drifted_allowances+=("$leg: allowance '${ALLOW_STALE[$leg]}' not evaluated (${UNEVALUATED[$leg]})")
        fi
    done
fi

rc=0
if [ "${#stale[@]}" -gt 0 ]; then
    printf 'pin-staleness: STALE: %s\n' "$(join_by ', ' "${stale[@]}")" >&2
    rc=1
fi
if [ "${#drifted_allowances[@]}" -gt 0 ]; then
    printf 'pin-staleness: STALE ALLOWANCE: %s\n' "$(join_by '; ' "${drifted_allowances[@]}")" >&2
    rc=1
fi
if [ "${#fetch_failed[@]}" -gt 0 ]; then
    printf 'pin-staleness: FETCH FAILED: %s\n' "$(join_by '; ' "${fetch_failed[@]}")" >&2
    rc=3
fi
if [ "${#unparseable[@]}" -gt 0 ]; then
    printf 'pin-staleness: PAYLOAD UNRECOGNISED: %s\n' "$(join_by '; ' "${unparseable[@]}")" >&2
    rc=3
fi
if [ "${#unreadable[@]}" -gt 0 ]; then
    printf 'pin-staleness: PIN UNREADABLE: %s\n' "$(join_by '; ' "${unreadable[@]}")" >&2
    rc=2
fi
if [ "${#unclassified[@]}" -gt 0 ]; then
    printf 'pin-staleness: UNCLASSIFIED DOCKER LEG: %s\n' "$(join_by '; ' "${unclassified[@]}")" >&2
    rc=2
fi
if [ "${#missing[@]}" -gt 0 ]; then
    printf 'pin-staleness: MISSING DOCKER LEG: %s\n' "$(join_by '; ' "${missing[@]}")" >&2
    rc=2
fi
[ "$rc" -eq 0 ] || exit "$rc"
if [ "${#allowed[@]}" -gt 0 ]; then
    printf 'pin-staleness: all %d pins are current or allowed (%s)\n' "${#CHECKED[@]}" "$(join_by ', ' "${allowed[@]}")"
else
    printf 'pin-staleness: all %d pins match the version their vendor serves today\n' "${#CHECKED[@]}"
fi
exit 0
