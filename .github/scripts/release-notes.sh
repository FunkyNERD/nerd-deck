#!/usr/bin/env bash
# Prints release notes in Markdown for a tag: every commit since the previous
# release of the same side (or since the start, for the first release),
# grouped by Conventional Commit type, each with its message body.
#
#   .github/scripts/release-notes.sh host-v1.2.3 [owner/repo]
#   .github/scripts/release-notes.sh firmware-v1.2.3 [owner/repo]
#
# The app (host-v*) and the firmware (firmware-v*) are released apart, so each
# only lists commits that touch its own folder (and the shared docs). Releases
# before the split were tagged v* and covered both; the first of each side
# counts from the last of those.
#
# With owner/repo, ends with a link to the full diff on GitHub.
set -euo pipefail

tag=${1:?usage: release-notes.sh <tag> [owner/repo]}
repo=${2:-}

case $tag in
    host-v*) side=host ;;
    firmware-v*) side=firmware ;;
    *) echo "release-notes.sh: $tag isn't host-v* or firmware-v*" >&2; exit 1 ;;
esac

prev=$(git describe --tags --abbrev=0 --match "$side-v*" "${tag}^" 2>/dev/null ||
    git describe --tags --abbrev=0 --match 'v*' "${tag}^" 2>/dev/null || true)
range=${prev:+$prev..}$tag

# Section order and headings; anything else lands in "Other changes".
types=(feat fix perf refactor docs ci build test style chore revert)
declare -A heading=(
    [feat]="Features" [fix]="Fixes" [perf]="Performance" [refactor]="Refactoring"
    [docs]="Documentation" [ci]="CI" [build]="Build" [test]="Tests" [style]="Style"
    [chore]="Chores" [revert]="Reverts" [other]="Other changes"
)
declare -A section

# Captured first so a git failure stops the script instead of reading as no commits.
log=$(git log --no-merges --format='%h%x1f%s%x1f%b%x1e' "$range" -- "$side" docs README.md)

# One record per commit: hash, subject, body, split on unit/record separators.
while IFS=$'\x1f' read -r -d $'\x1e' hash subject body; do
    hash=${hash#$'\n'}
    type=other
    text=$subject
    # type(scope)!: description
    if [[ $subject =~ ^([a-z]+)(\(([^\)]+)\))?(!)?:\ (.*)$ ]]; then
        if [[ -n ${heading[${BASH_REMATCH[1]}]:-} ]]; then
            type=${BASH_REMATCH[1]}
            text=${BASH_REMATCH[5]}
            [[ -n ${BASH_REMATCH[3]} ]] && text="**${BASH_REMATCH[3]}:** $text"
            [[ -n ${BASH_REMATCH[4]} ]] && text="**Breaking:** $text"
        fi
    fi
    entry="- $text ($hash)"$'\n'
    # The body minus trailers (Co-Authored-By: and friends), indented under the bullet.
    details=$(printf '%s\n' "$body" | grep -Ev '^[A-Za-z-]+: .+@' | sed -e '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}')
    if [[ -n $details ]]; then
        entry+=$(printf '%s\n' "$details" | sed 's/^/  /')$'\n'
    fi
    section[$type]+=$entry
done <<<"$log"

echo "## What's changed"
echo
if [[ -n $prev ]]; then echo "Everything since ${prev}."; else echo "First release."; fi
for type in "${types[@]}" other; do
    [[ -n ${section[$type]:-} ]] || continue
    echo
    echo "### ${heading[$type]}"
    echo
    printf '%s' "${section[$type]}"
done
if [[ -n $repo ]]; then
    echo
    if [[ -n $prev ]]; then
        echo "**Full diff:** https://github.com/$repo/compare/$prev...$tag"
    else
        echo "**Full history:** https://github.com/$repo/commits/$tag"
    fi
fi
