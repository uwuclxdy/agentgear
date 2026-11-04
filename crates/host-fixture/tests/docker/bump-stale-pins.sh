#!/usr/bin/env bash
# Turn a pin-staleness log's STALE summary into concrete pin bumps.
#
# The planner/editor behind `.github/workflows/pin-bump.yml`. It reads ONE thing: the
# `pin-staleness: STALE: leg (pin != current), ...` summary line of a saved
# pin-staleness.sh run. It never queries a vendor — pin-staleness.sh stays the single
# version source — and it never plans off the other summary classes: STALE ALLOWANCE,
# FETCH FAILED, PAYLOAD UNRECOGNISED, PIN UNREADABLE, UNCLASSIFIED DOCKER LEG and
# MISSING DOCKER LEG are a human's work (an allowance is a decision; a fetch failure or
# an unreadable payload names no current), so each is echoed loudly and ignored.
#
# A leg plans only when every check holds:
# - its pin line reads exactly as pin-staleness.sh reads it — `npm install -g <pkg>@<ver>`
#   for the NPM_PKG legs below, `^version=<ver>` for the version legs, one line each;
# - the pin on that line equals the log's pin: a disagreement means the log was not
#   written from this tree, and no bump off it is trustworthy;
# - the vendor current is not older than the pin — a downgrade is a vendor blip (a
#   rollback, a staged-rollout pointer), never something to commit. The compare orders
#   dotted numeric cores, so a suffix-only change (1.2.4 -> 1.2.4-beta.1) is never a
#   downgrade;
# - the bump moves the pin at all.
# A malformed or duplicated STALE line, and a summary class this file does not know, are
# refused the same way: a future class could name stale pins under a new name, and
# planning past it would open a PR from half a picture. Any refusal exits 1 with every
# reason printed and NOTHING written — no partial plan ships from a log that failed one
# of its checks; the run stays red and the next day's log decides again.
#
# The leg maps below mirror pin-staleness.sh's LEGS/NPM_PKG split by line shape. A leg
# the check names that is absent here refuses loudly instead of guessing its line shape:
# add it to both files in the same change.
#
# Modes: --dry-run prints the plan and writes nothing; the default applies it, rewriting
# each pin line in place through a temp-sibling rename.
#
# stdout carries only the machine-readable plan, one line per bump:
#   bump-stale-pins: PLAN: <leg> <old> <new>
# everything human-readable goes to stderr.
#
# exit codes: 0 a plan was produced (an empty one when the log names no stale pin);
# 1 the log names no safe bump — a refused leg, a malformed or duplicated STALE line, or
# a summary class this file does not know; nothing was written. 2 usage or IO trouble —
# an IO failure mid-apply can leave earlier legs written; the next run's log decides again.
#
# usage: bump-stale-pins.sh [--dry-run] <pin-staleness.log> [docker-dir]
#        (docker-dir default: this script's own directory)
set -euo pipefail   # planner: any refused leg or read must abort before the first write
unset CDPATH
export LC_ALL=C   # the log and the line shapes are ASCII; keep every grep and glob class exact

mode=apply
if [ "${1:-}" = --dry-run ]; then
    mode=dry-run
    shift
fi
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    printf 'bump-stale-pins: usage: bump-stale-pins.sh [--dry-run] <pin-staleness.log> [docker-dir]\n' >&2
    exit 2
fi
log=$1
docker_dir=${2:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
[ -f "$log" ] || { printf 'bump-stale-pins: no log at %s\n' "$log" >&2; exit 2; }
[ -d "$docker_dir" ] || { printf 'bump-stale-pins: no docker dir at %s\n' "$docker_dir" >&2; exit 2; }

CLEANUP=()
cleanup() {
    local p
    for p in "${CLEANUP[@]}"; do
        rm -rf -- "$p"
    done
}
trap cleanup EXIT

# The npm-installed legs, mirroring pin-staleness.sh's NPM_PKG: the pin is the `@<version>`
# on that package's one `npm install -g` line.
declare -A NPM_PKG=(
    [augment]=@augmentcode/auggie
    [copilot-cli]=@github/copilot
    [gemini]=@google/gemini-cli
    [kimi]=@moonshot-ai/kimi-code
    [qwen-code]=@qwen-code/qwen-code
    [crush]=@charmland/crush
)
# The legs pinning a bare `version=<version>` line, mirroring pin-staleness.sh's version
# case. Membership reads as ` <leg> ` inside the padded string, so a name can only match
# itself.
VERSION_LEGS=' antigravity-cli droid '

known_ignored() { # <class> -> 0 when the class is a loud-ignore one
    case "$1" in
        'STALE ALLOWANCE' | 'FETCH FAILED' | 'PAYLOAD UNRECOGNISED' | 'PIN UNREADABLE' | 'UNCLASSIFIED DOCKER LEG' | 'MISSING DOCKER LEG') return 0 ;;
    esac
    return 1
}

# Mirrors pin-staleness.sh's valid_version: the two must agree on what a pin is, or a
# bump could write a pin the check cannot read back.
valid_version() {
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

ver_core() { # <version> -> the dotted numeric core before any -/+ suffix
    printf '%s' "${1%%[-+]*}"
}

# version_lt A B: 0 when A's core is older than B's, component-wise with the shorter side
# zero-padded; 1 when it is not (equal cores included — a suffix-only change); 2 when the
# pair has no order this planner compares. Both sides are valid_version output, so every
# core component is digits.
version_lt() {
    local a b x y i n
    local -a pa pb
    a=$(ver_core "$1")
    b=$(ver_core "$2")
    IFS=. read -r -a pa <<< "$a"
    IFS=. read -r -a pb <<< "$b"
    n=$(( ${#pa[@]} > ${#pb[@]} ? ${#pa[@]} : ${#pb[@]} ))
    for ((i = 0; i < n; i++)); do
        x=${pa[i]:-0}
        y=${pb[i]:-0}
        case "$x$y" in
            *[!0-9]*) return 2 ;;
        esac
        # a leading 0 would read as octal in $(( )); 19+ digits wrap a signed 64-bit compare
        x=${x#"${x%%[!0]*}"}
        x=${x:-0}
        y=${y#"${y%%[!0]*}"}
        y=${y:-0}
        if [ "${#x}" -gt 18 ] || [ "${#y}" -gt 18 ]; then return 2; fi
        if (( 10#$x < 10#$y )); then return 0; fi
        if (( 10#$x > 10#$y )); then return 1; fi
    done
    return 1
}

plans=()    # 'leg old new', the machine-readable plan
targets=()  # file<TAB>line_no<TAB>new_line, the write half of the same rows
declare -A SEEN_LEG=()

plan_one() { # <entry> — one `leg (old != new)` item of the STALE summary
    local entry=$1 leg old new kind='' pkg='' file needle hits count pin_line pin line_no new_line rc=0
    local re='^([A-Za-z0-9._-]+) \(([0-9A-Za-z.+-]+) != ([0-9A-Za-z.+-]+)\)$'
    if [[ $entry =~ $re ]]; then
        leg=${BASH_REMATCH[1]}
        old=${BASH_REMATCH[2]}
        new=${BASH_REMATCH[3]}
    else
        printf 'bump-stale-pins: REFUSED: malformed STALE entry %q in %s\n' "$entry" "$log" >&2
        exit 1
    fi
    [ -z "${SEEN_LEG[$leg]:-}" ] || {
        printf 'bump-stale-pins: REFUSED: %q is named twice by the STALE summary\n' "$leg" >&2
        exit 1
    }
    SEEN_LEG[$leg]=1
    valid_version "$old" || { printf 'bump-stale-pins: REFUSED: %s: the log pin %q is not a version\n' "$leg" "$old" >&2; exit 1; }
    valid_version "$new" || { printf 'bump-stale-pins: REFUSED: %s: the log current %q is not a version\n' "$leg" "$new" >&2; exit 1; }

    if [ -n "${NPM_PKG[$leg]:-}" ]; then
        kind=npm
        pkg=${NPM_PKG[$leg]}
    elif [[ $VERSION_LEGS == *" $leg "* ]]; then
        kind=version
    else
        printf 'bump-stale-pins: REFUSED: %s: no version source in this planner; add the leg beside pin-staleness.sh'"'"'s LEGS and the maps here\n' "$leg" >&2
        exit 1
    fi

    if [ "$old" = "$new" ]; then
        printf 'bump-stale-pins: REFUSED: %s: the log current equals its pin (%s); nothing to bump\n' "$leg" "$old" >&2
        exit 1
    fi
    version_lt "$new" "$old" || rc=$?
    case $rc in
        0) printf 'bump-stale-pins: REFUSED: %s: downgrade — the vendor serves %s, older than the pin %s\n' "$leg" "$new" "$old" >&2; exit 1 ;;
        2) printf 'bump-stale-pins: REFUSED: %s: the pair %s -> %s has no dotted-numeric order\n' "$leg" "$old" "$new" >&2; exit 1 ;;
    esac

    file="$docker_dir/$leg/Dockerfile"
    [ -f "$file" ] || { printf 'bump-stale-pins: REFUSED: %s: no Dockerfile at %s\n' "$leg" "$file" >&2; exit 1; }
    if [ "$kind" = npm ]; then
        needle="npm install -g $pkg@"
        hits=$(grep -nF -- "$needle" "$file" || true)
    else
        needle='^version='
        hits=$(grep -nE -- "$needle" "$file" || true)
    fi
    count=$(printf '%s' "$hits" | grep -c . || true)
    [ "$count" -eq 1 ] || {
        printf "bump-stale-pins: REFUSED: %s: expected exactly one '%s<version>' line in %s, found %s\n" "$leg" "$needle" "$file" "$count" >&2
        exit 1
    }
    line_no=${hits%%:*}
    pin_line=${hits#*:}
    if [ "$kind" = npm ]; then
        pin=${pin_line#*"$needle"}
    else
        pin=${pin_line#version=}
    fi
    pin=${pin%%[[:space:]]*}
    valid_version "$pin" || { printf 'bump-stale-pins: REFUSED: %s: unparseable version %q on the pin line in %s\n' "$leg" "$pin" "$file" >&2; exit 1; }
    [ "$pin" = "$old" ] || {
        printf 'bump-stale-pins: REFUSED: %s: the log pins %s but %s pins %s; the log is not from this tree\n' "$leg" "$old" "$file" "$pin" >&2
        exit 1
    }
    if [ "$kind" = npm ]; then
        new_line=${pin_line/"npm install -g $pkg@$old"/"npm install -g $pkg@$new"}
    else
        new_line="version=$new${pin_line#"version=$old"}"
    fi
    [ "$new_line" != "$pin_line" ] || { printf 'bump-stale-pins: REFUSED: %s: the pin edit did not apply in %s\n' "$leg" "$file" >&2; exit 1; }
    plans+=("$leg $old $new")
    targets+=("$file"$'\t'"$line_no"$'\t'"$new_line")
    printf 'bump-stale-pins: planned %s: %s -> %s\n' "$leg" "$old" "$new" >&2
}

apply_one() { # <file> <line_no> <new_line> — rewrite one pin line through a temp sibling
    local file=$1 line_no=$2 new_line=$3
    local -a lines=()
    local total had_nl i tmp
    mapfile -t lines < "$file"
    total=${#lines[@]}
    [ "$total" -ge 1 ] && [ "$line_no" -ge 1 ] && [ "$line_no" -le "$total" ] || {
        printf 'bump-stale-pins: line %s of %s is out of range (%s lines); refusing to write\n' "$line_no" "$file" "$total" >&2
        exit 1
    }
    had_nl=1
    if [ -n "$(tail -c 1 -- "$file")" ]; then had_nl=0; fi
    lines[line_no - 1]=$new_line
    tmp=$(mktemp -- "$file.bumpXXXXXX") || { printf 'bump-stale-pins: cannot create a temp sibling for %s\n' "$file" >&2; exit 2; }
    CLEANUP+=("$tmp")
    for ((i = 0; i < total; i++)); do
        if [ "$i" -eq $((total - 1)) ] && [ "$had_nl" -eq 0 ]; then
            printf '%s' "${lines[i]}"
        else
            printf '%s\n' "${lines[i]}"
        fi
    done > "$tmp"
    chmod --reference="$file" "$tmp"
    mv -f -- "$tmp" "$file"
    printf 'bump-stale-pins: wrote %s\n' "$file" >&2
}

# One pass over every summary line the check wrote: the classes that are a human's work
# are echoed loudly, an unknown class refuses the run, and the one STALE line is kept.
# grep rc 1 is an honest empty set (a green check writes no summary line); rc 2 is an
# unreadable log, which must not read as an empty plan.
grep_rc=0
summary_lines=$(grep '^pin-staleness: [A-Z][A-Z ]*: ' "$log") || grep_rc=$?
[ "$grep_rc" -le 1 ] || { printf 'bump-stale-pins: cannot read the log at %s\n' "$log" >&2; exit 2; }
stale_line=''
stale_count=0
while IFS= read -r line; do
    [ -n "$line" ] || continue
    cls=${line#'pin-staleness: '}
    cls=${cls%%: *}
    if [ "$cls" = STALE ]; then
        stale_count=$((stale_count + 1))
        stale_line=$line
        continue
    fi
    if known_ignored "$cls"; then
        printf 'bump-stale-pins: ignored, human work, never planned: %s\n' "$line" >&2
    else
        printf 'bump-stale-pins: REFUSED: summary class %q in %s is not one this planner knows; add it beside known_ignored before planning past it\n' "$cls" "$log" >&2
        exit 1
    fi
done <<< "$summary_lines"
[ "$stale_count" -le 1 ] || {
    printf 'bump-stale-pins: REFUSED: %s STALE summary lines in %s, expected at most one\n' "$stale_count" "$log" >&2
    exit 1
}

if [ "$stale_count" -eq 1 ]; then
    payload=${stale_line#'pin-staleness: STALE: '}
    [ -n "$payload" ] || { printf 'bump-stale-pins: REFUSED: a STALE summary with no entries in %s\n' "$log" >&2; exit 1; }
    # Entries join with ', ' and no field can hold that pair, so a literal replace splits
    # them exactly; IFS-splitting would cut inside `leg (old != new)` at every space.
    entries=${payload//', '/$'\n'}
    while IFS= read -r entry; do
        [ -n "$entry" ] || continue
        plan_one "$entry"
    done <<< "$entries"
fi

if [ "$mode" = apply ]; then
    for row in "${targets[@]}"; do
        file=${row%%$'\t'*}
        rest=${row#*$'\t'}
        line_no=${rest%%$'\t'*}
        new_line=${rest#*$'\t'}
        apply_one "$file" "$line_no" "$new_line"
    done
fi

for row in "${plans[@]}"; do
    read -r leg old new <<< "$row"
    printf 'bump-stale-pins: PLAN: %s %s %s\n' "$leg" "$old" "$new"
done
if [ "${#plans[@]}" -eq 0 ]; then
    printf 'bump-stale-pins: plan is empty; nothing to bump\n' >&2
elif [ "$mode" = apply ] && [ "${#targets[@]}" -gt 0 ]; then
    printf 'bump-stale-pins: planned %s bump(s), written in place\n' "${#plans[@]}" >&2
else
    printf 'bump-stale-pins: planned %s bump(s), dry run, nothing written\n' "${#plans[@]}" >&2
fi
