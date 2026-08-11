#!/usr/bin/env bash
# Regression tests for release.sh --abort's dirty-at-start guard.
set -euo pipefail

SCRIPT_UNDER_TEST="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-release-abort-XXXXXX")
trap 'rm -rf "$work"' EXIT

make_fixture() {
    local repo=$1
    mkdir -p "$repo/scripts"
    cp "$SCRIPT_UNDER_TEST" "$repo/scripts/release.sh"
    chmod +x "$repo/scripts/release.sh"
    git -C "$repo" init -q
    printf 'version = "0.1.0"\n' > "$repo/Cargo.toml"
    printf '## [Unreleased]\n' > "$repo/CHANGELOG.md"
    printf 'clean\n' > "$repo/unrelated.txt"
    git -C "$repo" add Cargo.toml CHANGELOG.md unrelated.txt scripts/release.sh
}

# Pre-existing modified and untracked files must make --abort refuse before
# checkout/rm, leaving both the operator edits and release-surface edits intact.
guarded="$work/guarded"
mkdir -p "$guarded"
make_fixture "$guarded"
printf 'operator edit\n' > "$guarded/unrelated.txt"
printf 'version = "0.2.0"\n' > "$guarded/Cargo.toml"
printf 'release notes\n' > "$guarded/release-notes-v0.2.0.md"
if (cd "$guarded" && scripts/release.sh --abort >stdout 2>stderr); then
    echo "guarded abort unexpectedly succeeded" >&2
    exit 1
fi
grep -F 'working tree dirty outside the release surfaces' "$guarded/stderr" >/dev/null
test "$(cat "$guarded/unrelated.txt")" = 'operator edit'
test "$(cat "$guarded/Cargo.toml")" = 'version = "0.2.0"'
test -f "$guarded/release-notes-v0.2.0.md"

# With only release-surface changes, --abort may revert the staged-file edit
# and remove the untracked release-notes file.
allowed="$work/allowed"
mkdir -p "$allowed"
make_fixture "$allowed"
printf 'version = "0.2.0"\n' > "$allowed/Cargo.toml"
printf 'release notes\n' > "$allowed/release-notes-v0.2.0.md"
(cd "$allowed" && scripts/release.sh --abort >stdout 2>stderr)
test "$(cat "$allowed/Cargo.toml")" = 'version = "0.1.0"'
test ! -e "$allowed/release-notes-v0.2.0.md"

echo "release --abort regression tests: ok"
