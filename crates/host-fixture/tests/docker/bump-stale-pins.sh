#!/usr/bin/env bash
# Turn a pin-staleness log's STALE summary into concrete pin bumps.
#
# The planner/editor behind `.github/workflows/pin-bump.yml`. It reads two things from a
# saved pin-staleness.sh run: the `pin-staleness: STALE: leg (pin != current), ...`
# summary line, and each `pin-staleness: BUMP: leg version url sha512` payload that
# carries a multi-field pin's whole triple. It never queries a vendor — pin-staleness.sh
# stays the single version source — and it never plans off the other summary classes:
# STALE ALLOWANCE, FETCH FAILED, PAYLOAD UNRECOGNISED, PIN UNREADABLE, UNCLASSIFIED
# DOCKER LEG and MISSING DOCKER LEG are a human's work (an allowance is a decision; a
# fetch failure or an unreadable payload names no current), so each is echoed loudly and
# ignored.
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
# - the bump moves the pin at all;
# - a MULTI_FIELD_LEGS leg pins more than its `version=` line (antigravity-cli also pins
#   the artifact `url=` and its `sha512=`), so it plans only from a BUMP payload: the
#   payload version must equal the STALE current, the url must be a plain https URL, the
#   sha512 a 128-character lowercase hex digest, and the Dockerfile must carry exactly
#   one `url=` and one `sha512=` line. All three lines move together in one plan;
# - the part of a line the plan rewrites but does not replace holds no second copy of the
#   old pin either: the exact text the edit overwrites is masked out (the first
#   `npm install -g <pkg>@<old>` on an npm pin line, the leading `version=<old>` on a
#   version pin line, the whole `url=<old url>` / `sha512=<old sha>` on a multi-field
#   leg). A repeat found there refuses as `REFUSED: <leg>: pin <old> appears again on line
#   <n> of <file>, beside the pin this planner moves`. An inline `# ...` tail counts: this
#   planner does not parse shell quoting, so it cannot tell a tail from a quoted `#` with
#   a live install after it, and a tail repeating the version goes stale on every bump;
# - no line of the Dockerfile other than the ones the plan rewrites (the pin line, plus
#   `url=` and `sha512=` for a multi-field leg) still holds the old pin as a whole version
#   token: a pin repeated on a line this planner does not move (a version assert, a
#   second install) would ship a half-bumped leg. The old sha512 of a multi-field pin
#   refuses the same way, and so does the package itself (a word equal to `<pkg>` or
#   `<pkg>@...`) anywhere else, the pin line included, because a second install at any
#   version wins over the moved pin. A comment line (its first non-blank characters are
#   `#` or `//`) is not scanned, and a whole token has no digit or `<digit>.` before it
#   and no digit or `.<digit>` after it, so `1.3.0.5`, `0.1.3.0` and `21.3.0` do not hold
#   `1.3.0` while `copilot.1.3.0.tgz` does. An unrelated token equal to the pin refuses
#   too: this planner cannot tell it from a repeat. This is an early refusal over literal
#   text and does not see: a version split across lines (a `\` continuation or a split
#   quoted string), another spelling of it (a regex-escaped `1\.3\.0`, the build id inside
#   a multi-field url, a name ending in a digit such as `node22.1.3.0.tgz`), the old pin
#   inside a new BUMP url, the package in any other form (quoted, `<pkg>;`, a tarball url,
#   an npm alias, a variable), the old sha512 beside a moved field, or a pin on a line
#   whose first non-blank characters are `#` or `//` wherever that line sits, a heredoc
#   or a multi-line string included. An assert or a BUMP url among those reds a leg in
#   the matrix; a second install in another form would not, because no npm leg compares
#   what it installed with its pin line.
# A malformed or duplicated STALE line, a malformed or duplicated BUMP line, a BUMP
# payload for a leg the STALE summary does not name or whose pin is single-field, and a
# summary class this file does not know, are refused the same way: a future class could
# name stale pins under a new name, and planning past it would open a PR from half a
# picture. Any refusal exits 1 naming its reason and with NOTHING written — no
# partial plan ships from a log that failed one of its checks; the run stays red and the
# next day's log decides again.
#
# The leg maps below mirror pin-staleness.sh's LEGS/NPM_PKG split by line shape;
# MULTI_FIELD_LEGS marks the legs whose pin spans more than `version=`, whose whole
# triple the check reports on a BUMP line. A leg the check names that is absent here
# refuses loudly instead of guessing its line shape: add it to both files in the same
# change.
#
# Modes: --dry-run prints the plan and writes nothing; the default applies it, rewriting
# each pin line in place through a temp-sibling rename.
#
# stdout carries only the machine-readable plan, one line per bump:
#   bump-stale-pins: PLAN: <leg> <old> <new>
# everything human-readable goes to stderr.
#
# exit codes: 0 a plan was produced (an empty one when the log names no stale pin);
# 1 the log names no safe bump — a refused leg, a malformed or duplicated STALE or BUMP
# line, a missing BUMP payload for a multi-field pin, or a summary class this file does
# not know; nothing was written. 2 usage or IO trouble — an IO failure mid-apply can
# leave earlier legs written; the next run's log decides again.
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
# The legs whose pin spans lines beyond `version=`: the check reports their whole triple
# on a BUMP line, and a plan moves every field or refuses.
MULTI_FIELD_LEGS=' antigravity-cli '

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

# holds_version <line> <ver>: 0 when <line> holds <ver> as a whole version token — a
# literal match (a `.` or `+` in <ver> is that character) with no digit or `<digit>.`
# before it and no digit or `.<digit>` after it. Every occurrence is tried, so a longer
# version earlier on the line cannot hide a real one later.
holds_version() {
    local rest=$1 ver=$2 seen='' head tail
    while [[ $rest == *"$ver"* ]]; do
        head=${rest%%"$ver"*}
        tail=${rest#*"$ver"}
        case "$seen$head" in
            *[0123456789] | *[0123456789].) ;;
            *)
                case "$tail" in
                    [0123456789]* | .[0123456789]*) ;;
                    *) return 0 ;;
                esac
                ;;
        esac
        # step one character past this match's start, so an overlapping match is tried too
        seen=$seen$head${ver:0:1}
        rest=${ver:1}$tail
    done
    return 1
}

# names_package <text> <pkg>: 0 when a word of <text> is <pkg> or <pkg>@<anything>.
names_package() {
    local word
    local -a words=()
    read -r -a words <<< "$1" || true
    for word in "${words[@]}"; do
        if [ "$word" = "$2" ] || [[ $word == "$2@"* ]]; then return 0; fi
    done
    return 1
}

plans=()    # 'leg old new', the machine-readable plan
targets=()  # file<TAB>line_no<TAB>new_line, the write half of the same rows
declare -A SEEN_LEG=()
declare -A BUMP=()  # leg -> 'version url sha512' from that leg's BUMP payload

plan_one() { # <entry> — one `leg (old != new)` item of the STALE summary
    local entry=$1 leg old new kind='' pkg='' file needle hits count pin_line pin line_no new_line rc=0
    local multi=0 bump_version bump_url bump_sha url_no url_line old_url url_tail sha_no sha_line old_sha sha_tail
    local scan_no scan_line kept_i
    local -a scan_lines=() kept_texts=() kept_nos=()
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
    if [[ $MULTI_FIELD_LEGS == *" $leg "* ]]; then
        multi=1
        [ -n "${BUMP[$leg]:-}" ] || {
            printf 'bump-stale-pins: REFUSED: %s: the log carries no BUMP payload for its three-field pin; re-run the check\n' "$leg" >&2
            exit 1
        }
        read -r bump_version bump_url bump_sha <<< "${BUMP[$leg]}" || true
        [ "$bump_version" = "$new" ] || {
            printf 'bump-stale-pins: REFUSED: %s: BUMP payload version %s != the STALE current %s\n' "$leg" "$bump_version" "$new" >&2
            exit 1
        }
        hits=$(grep -nE '^url=' "$file" || true)
        count=$(printf '%s' "$hits" | grep -c . || true)
        [ "$count" -eq 1 ] || {
            printf "bump-stale-pins: REFUSED: %s: expected exactly one '^url=<url>' line in %s, found %s\n" "$leg" "$file" "$count" >&2
            exit 1
        }
        url_no=${hits%%:*}
        url_line=${hits#*:}
        old_url=${url_line#url=}
        old_url=${old_url%%[[:space:]]*}
        url_tail=${url_line#"url=$old_url"}
        hits=$(grep -nE '^sha512=' "$file" || true)
        count=$(printf '%s' "$hits" | grep -c . || true)
        [ "$count" -eq 1 ] || {
            printf "bump-stale-pins: REFUSED: %s: expected exactly one '^sha512=<digest>' line in %s, found %s\n" "$leg" "$file" "$count" >&2
            exit 1
        }
        sha_no=${hits%%:*}
        sha_line=${hits#*:}
        old_sha=${sha_line#sha512=}
        old_sha=${old_sha%%[[:space:]]*}
        sha_tail=${sha_line#"sha512=$old_sha"}
    fi
    # The old pin beside the one this plan rewrites would survive the bump too: scan the
    # part of each moved line the rewrite leaves alone.
    if [ "$kind" = npm ]; then
        kept_texts+=("${pin_line/"npm install -g $pkg@$old"/}")
    else
        kept_texts+=("${pin_line#"version=$old"}")
    fi
    kept_nos+=("$line_no")
    if [ "$multi" -eq 1 ]; then
        kept_texts+=("$url_tail" "$sha_tail")
        kept_nos+=("$url_no" "$sha_no")
    fi
    for kept_i in "${!kept_texts[@]}"; do
        holds_version "${kept_texts[kept_i]}" "$old" || continue
        printf 'bump-stale-pins: REFUSED: %s: pin %s appears again on line %s of %s, beside the pin this planner moves\n' "$leg" "$old" "${kept_nos[kept_i]}" "$file" >&2
        exit 1
    done
    # A second install of the package at any version would win over the moved pin, and a
    # leg reading the installed version back would agree with it.
    if [ "$kind" = npm ] && names_package "${pin_line/"npm install -g $pkg@$old"/}" "$pkg"; then
        printf 'bump-stale-pins: REFUSED: %s: package %s appears again on line %s of %s; this planner moves one install\n' "$leg" "$pkg" "$line_no" "$file" >&2
        exit 1
    fi
    if [ "$multi" -eq 1 ] && [ -z "$old_sha" ]; then
        printf 'bump-stale-pins: REFUSED: %s: the sha512= line in %s holds no digest\n' "$leg" "$file" >&2
        exit 1
    fi
    # The old pin on any live line this plan does not rewrite would survive the bump.
    mapfile -t scan_lines < "$file"
    for ((scan_no = 1; scan_no <= ${#scan_lines[@]}; scan_no++)); do
        [ "$scan_no" -ne "$line_no" ] || continue
        if [ "$multi" -eq 1 ]; then
            [ "$scan_no" -ne "$url_no" ] && [ "$scan_no" -ne "$sha_no" ] || continue
        fi
        scan_line=${scan_lines[scan_no - 1]}
        scan_line=${scan_line#"${scan_line%%[![:space:]]*}"}
        case "$scan_line" in
            '#'* | '//'*) continue ;;
        esac
        if [ "$multi" -eq 1 ] && [[ $scan_line == *"$old_sha"* ]]; then
            printf 'bump-stale-pins: REFUSED: %s: the old sha512 also appears on line %s of %s, a line this planner does not move\n' "$leg" "$scan_no" "$file" >&2
            exit 1
        fi
        if [ "$kind" = npm ] && names_package "$scan_line" "$pkg"; then
            printf 'bump-stale-pins: REFUSED: %s: package %s also appears on line %s of %s; this planner moves one install\n' "$leg" "$pkg" "$scan_no" "$file" >&2
            exit 1
        fi
        holds_version "$scan_line" "$old" || continue
        printf 'bump-stale-pins: REFUSED: %s: pin %s also appears on line %s of %s, a line this planner does not move\n' "$leg" "$old" "$scan_no" "$file" >&2
        exit 1
    done
    plans+=("$leg $old $new")
    targets+=("$file"$'\t'"$line_no"$'\t'"$new_line")
    if [ "$multi" -eq 1 ]; then
        targets+=("$file"$'\t'"$url_no"$'\t'"url=$bump_url$url_tail")
        targets+=("$file"$'\t'"$sha_no"$'\t'"sha512=$bump_sha$sha_tail")
    fi
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
    if ! {
        for ((i = 0; i < total; i++)); do
            if [ "$i" -eq $((total - 1)) ] && [ "$had_nl" -eq 0 ]; then
                printf '%s' "${lines[i]}"
            else
                printf '%s\n' "${lines[i]}"
            fi
        done > "$tmp"
    }; then
        printf 'bump-stale-pins: IO: cannot write a temp sibling for %s; earlier pins may already be written\n' "$file" >&2
        exit 2
    fi
    if ! chmod --reference="$file" "$tmp"; then
        printf 'bump-stale-pins: IO: cannot match the mode of %s on its temp sibling; earlier pins may already be written\n' "$file" >&2
        exit 2
    fi
    if ! mv -f -- "$tmp" "$file"; then
        printf 'bump-stale-pins: IO: cannot replace %s; earlier pins may already be written\n' "$file" >&2
        exit 2
    fi
    printf 'bump-stale-pins: wrote %s\n' "$file" >&2
}

# One pass over every summary line the check wrote: the classes that are a human's work
# are echoed loudly, an unknown class refuses the run, the one STALE line is kept, and
# each BUMP payload is validated into the BUMP map.
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
    if [ "$cls" = BUMP ]; then
        bump_payload=${line#'pin-staleness: BUMP: '}
        fields=()
        read -r -a fields <<< "$bump_payload" || true
        if [ "${#fields[@]}" -ne 4 ]; then
            printf 'bump-stale-pins: REFUSED: malformed BUMP line: %s\n' "$line" >&2
            exit 1
        fi
        leg=${fields[0]}
        bversion=${fields[1]}
        burl=${fields[2]}
        bsha=${fields[3]}
        if [[ $MULTI_FIELD_LEGS != *" $leg "* ]]; then
            printf 'bump-stale-pins: REFUSED: %s: a BUMP payload names a leg this planner has no three-field pin for\n' "$leg" >&2
            exit 1
        fi
        if [ -n "${BUMP[$leg]:-}" ]; then
            printf 'bump-stale-pins: REFUSED: %s: named twice by the BUMP payload\n' "$leg" >&2
            exit 1
        fi
        valid_version "$bversion" || { printf 'bump-stale-pins: REFUSED: %s: BUMP version %s is not a version\n' "$leg" "$bversion" >&2; exit 1; }
        case "$burl" in
            https://*[!A-Za-z0-9./:_-]* | https://) bad=1 ;;
            https://*) bad=0 ;;
            *) bad=1 ;;
        esac
        if [ "$bad" -ne 0 ]; then
            printf 'bump-stale-pins: REFUSED: %s: BUMP url %s is not a plain https url\n' "$leg" "$burl" >&2
            exit 1
        fi
        case "$bsha" in
            *[!0-9a-f]*) bad=1 ;;
            *) bad=0 ;;
        esac
        if [ "$bad" -ne 0 ] || [ "${#bsha}" -ne 128 ]; then
            printf 'bump-stale-pins: REFUSED: %s: BUMP sha512 %s is not a 128-character lowercase hex digest\n' "$leg" "$bsha" >&2
            exit 1
        fi
        BUMP[$leg]="$bversion $burl $bsha"
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
# A BUMP payload for a leg no STALE entry names carries a triple nothing plans: the
# summary and the payload disagree, so no plan from this log is whole.
for leg in "${!BUMP[@]}"; do
    [ -z "${SEEN_LEG[$leg]:-}" ] || continue
    printf 'bump-stale-pins: REFUSED: %s: a BUMP payload names a leg the STALE summary does not\n' "$leg" >&2
    exit 1
done

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
