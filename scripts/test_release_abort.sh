#!/usr/bin/env bash
# Regression tests for the monorepo release.sh --abort dirty-at-start guard.
set -euo pipefail

SCRIPT_UNDER_TEST="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-release-abort-XXXXXX")
trap 'rm -rf "$work"' EXIT

make_fixture() {
    local repo=$1
    mkdir -p "$repo/dracon-system/scripts"
    git -C "$repo" init -q -b main
    git -C "$repo" config core.hooksPath /dev/null
    git -C "$repo" config user.name fixture
    git -C "$repo" config user.email fixture@example.test
    cp "$SCRIPT_UNDER_TEST" "$repo/dracon-system/scripts/release.sh"
    chmod +x "$repo/dracon-system/scripts/release.sh"
    cat > "$repo/.gitignore" <<'EOF'
target/
dracon-system/
EOF
    cat > "$repo/Cargo.toml" <<'EOF'
[workspace]
members = ["dracon-system"]
resolver = "2"
EOF
    printf '[package]\nname = "dracon-system"\nversion = "0.1.0"\nedition = "2021"\n' \
        > "$repo/dracon-system/Cargo.toml"
    printf 'version = 4\n\n[[package]]\nname = "dracon-system"\nversion = "0.1.0"\n' \
        > "$repo/Cargo.lock"
    printf '## [Unreleased]\n' > "$repo/dracon-system/CHANGELOG.md"
    printf 'clean\n' > "$repo/unrelated.txt"
    git -C "$repo" add .
    git -C "$repo" add -f -- dracon-system
    git -C "$repo" commit -qm init
}

# Pre-existing modified and untracked files must make --abort refuse before
# restore/rm, leaving both operator edits and release-surface edits intact.
guarded="$work/guarded"
mkdir -p "$guarded"
make_fixture "$guarded"
printf 'operator edit\n' > "$guarded/unrelated.txt"
printf 'version = "0.2.0"\n' > "$guarded/dracon-system/Cargo.toml"
printf 'broken lock\n' > "$guarded/Cargo.lock"
printf 'release notes\n' > "$guarded/dracon-system/release-notes-v0.2.0.md"
if (cd "$guarded" && dracon-system/scripts/release.sh --abort \
        >"$work/guarded.stdout" 2>"$work/guarded.stderr"); then
    echo "guarded abort unexpectedly succeeded" >&2
    exit 1
fi
grep -F 'working tree dirty outside the release surfaces' "$work/guarded.stderr" >/dev/null
test "$(cat "$guarded/unrelated.txt")" = 'operator edit'
test "$(cat "$guarded/dracon-system/Cargo.toml")" = 'version = "0.2.0"'
test "$(cat "$guarded/Cargo.lock")" = 'broken lock'
test -f "$guarded/dracon-system/release-notes-v0.2.0.md"

# With only release-surface changes, --abort may restore tracked files and
# remove the ignored, untracked release-notes file.
allowed="$work/allowed"
mkdir -p "$allowed"
make_fixture "$allowed"
printf '[package]\nname = "dracon-system"\nversion = "0.2.0"\nedition = "2021"\n' \
    > "$allowed/dracon-system/Cargo.toml"
printf 'changed lock\n' > "$allowed/Cargo.lock"
printf 'release notes\n' > "$allowed/dracon-system/release-notes-v0.2.0.md"
(cd "$allowed" && dracon-system/scripts/release.sh --abort \
    >"$work/allowed.stdout" 2>"$work/allowed.stderr")
grep -F 'local modifications reverted' "$work/allowed.stdout" >/dev/null
test "$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "$allowed/dracon-system/Cargo.toml")" = 0.1.0
test "$(head -1 "$allowed/Cargo.lock")" = 'version = 4'
test ! -e "$allowed/dracon-system/release-notes-v0.2.0.md"
test -z "$(git -C "$allowed" status --porcelain)"

echo "release --abort regression tests: ok"
