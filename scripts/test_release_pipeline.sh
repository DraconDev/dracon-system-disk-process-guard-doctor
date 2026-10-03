#!/usr/bin/env bash
# Regression test for the standalone-repo release pipeline's gates, fixture, rerun
# branches, and mirror-tag reminder. All external release commands are stubs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-release-pipeline-XXXXXX")
trap 'rm -rf "$work"' EXIT

repo="$work/repo"
mkdir -p "$repo/dracon-system/scripts" "$work/bin" "$work/home/.cargo"
git init -q -b main "$repo"
git -C "$repo" config core.hooksPath /dev/null
git -C "$repo" config user.name fixture
git -C "$repo" config user.email fixture@example.test

git init -q --bare "$work/origin.git"
git init -q --bare "$work/gitlab.git"
git -C "$repo" remote add origin "$work/origin.git"
git -C "$repo" remote add gitlab "$work/gitlab.git"

cp "$SCRIPT_DIR/release.sh" "$repo/dracon-system/scripts/release.sh"
cp "$SCRIPT_DIR/close-changelog.py" "$repo/dracon-system/scripts/close-changelog.py"
cp "$SCRIPT_DIR/verify-install.sh" "$repo/dracon-system/scripts/verify-install.sh"
cp "$SCRIPT_DIR/resolve-github-remote.sh" "$repo/dracon-system/scripts/resolve-github-remote.sh"
chmod +x "$repo/dracon-system/scripts"/*
cat > "$repo/.gitignore" <<'EOF'
target/
dracon-system/
.publish-count
.gh-release
EOF
cat > "$repo/Cargo.toml" <<'EOF'
[workspace]
members = ["dracon-system"]
resolver = "2"
EOF
cat > "$repo/dracon-system/Cargo.toml" <<'EOF'
[package]
name = "dracon-system"
version = "0.0.0"
edition = "2021"
EOF
cat > "$repo/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "dracon-system"
version = "0.0.0"
EOF
cat > "$repo/dracon-system/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

- fixture
EOF

git -C "$repo" add .
git -C "$repo" add -f -- dracon-system
git -C "$repo" commit -qm init

cat > "$work/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
root="${DRACON_FIXTURE_ROOT:?}"
package="$root/target/package/dracon-system-0.1.0"
case "${1:-}" in
    metadata)
        printf '{"workspace_root":"%s"}\n' "$root"
        ;;
    check)
        version=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "$root/dracon-system/Cargo.toml")
        sed -i "/^name = \"dracon-system\"$/{n;s/^version = .*/version = \"$version\"/;}" "$root/Cargo.lock"
        ;;
    publish)
        mkdir -p "$package"
        if [[ " $* " == *' --dry-run '* ]]; then
            exit 0
        fi
        count_file="$root/.publish-count"
        count=0
        [[ -f "$count_file" ]] && count=$(cat "$count_file")
        count=$((count + 1))
        printf '%s\n' "$count" > "$count_file"
        if [[ $count -gt 1 ]]; then
            echo 'error: crate already exists on crates.io index' >&2
            exit 101
        fi
        ;;
    install)
        install_root=""
        while [[ $# -gt 0 ]]; do
            if [[ "$1" == --root ]]; then
                install_root=$2
                shift 2
            else
                shift
            fi
        done
        mkdir -p "$install_root/bin"
        cat > "$install_root/bin/dracon-system" <<'BIN'
#!/usr/bin/env bash
if [[ "${1:-}" == --version ]]; then
    echo 'dracon-system 0.1.0'
elif [[ "${1:-}" == status && "${2:-}" == --json ]]; then
    echo '{"system_root":"/tmp/.dracon","nixos_root":"/tmp/.dracon/nixos","sync_policy":"/tmp/sync.toml","system_policy":"/tmp/system.toml","system_policy_exists":false,"sync_service_active":false}'
else
    exit 2
fi
BIN
        chmod +x "$install_root/bin/dracon-system"
        ;;
    test|build|clippy|deny)
        ;;
    *)
        echo "unexpected cargo invocation: $*" >&2
        exit 2
        ;;
esac
EOF
cat > "$work/bin/cargo-deny" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == check ]]
EOF
cat > "$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
state="${DRACON_FIXTURE_ROOT:?}/.gh-release"
case "${1:-}" in
    auth)
        [[ "${2:-}" == status ]]
        ;;
    release)
        case "${2:-}" in
            view)
                [[ -f "$state" ]]
                ;;
            create)
                : > "$state"
                ;;
            *)
                echo "unexpected gh release invocation: $*" >&2
                exit 2
                ;;
        esac
        ;;
    *)
        echo "unexpected gh invocation: $*" >&2
        exit 2
        ;;
esac
EOF
chmod +x "$work/bin/cargo" "$work/bin/cargo-deny" "$work/bin/gh"
touch "$work/home/.cargo/credentials.toml"

run_release() {
    # R4-M-10: the legacy fixture names no github remote (local bare
    # paths), so it pins the explicit --remote override path.
    DRACON_FIXTURE_ROOT="$repo" HOME="$work/home" PATH="$work/bin:$PATH" \
        timeout 180 "$repo/dracon-system/scripts/release.sh" 0.1.0 --remote origin --yes
}

first_output="$work/first.out"
# AUDIT 2026-10-01: stdout and stderr go into ONE capture file, and a failed
# assertion prints it. This script used to exit 1 with no output at all,
# because the release stderr landed in a file the EXIT trap deleted — a stale
# fixture was then indistinguishable from a real tooling regression.
current_capture="$first_output"
fail() {
    echo "release pipeline regression FAILED: $*" >&2
    echo "--- last 30 lines of the release output ---" >&2
    tail -30 "$current_capture" >&2
    exit 1
}
assert_contains() { grep -F "$1" "$current_capture" >/dev/null || fail "expected output to contain: $1"; }

run_release >"$first_output" 2>&1
assert_contains 'all gates passed'
assert_contains 'fixture check on packaged artifact'
assert_contains 'mirror remotes get main from the daemon'
# Tag convention is nested-repo: dracon-system-vX.Y.Z (AGENTS.md), NOT bare vX.Y.Z.
assert_contains 'git push gitlab dracon-system-v0.1.0'
assert_contains '✓ dracon-system v0.1.0 released'
assert_contains "push remote: origin (explicit --remote override)"

test "$(git -C "$repo" tag --list dracon-system-v0.1.0)" = dracon-system-v0.1.0
git --git-dir="$work/origin.git" rev-parse refs/heads/main >/dev/null
git --git-dir="$work/origin.git" rev-parse refs/tags/dracon-system-v0.1.0 >/dev/null

second_output="$work/second.out"
current_capture="$second_output"
run_release >"$second_output" 2>&1
assert_contains 'already published; continuing'
assert_contains 'nothing to commit (release commit already exists)'
assert_contains 'tag dracon-system-v0.1.0 already exists'
assert_contains 'github release dracon-system-v0.1.0 already exists'

test "$(cat "$repo/.publish-count")" = 2
test "$(git -C "$repo" log --format=%s -1)" = 'release: v0.1.0'
test "$(git -C "$repo" status --porcelain)" = ''
test -f "$repo/Cargo.lock"

# The version rewrite must edit the [package] line and keep the header: a
# rewrite that dropped `version = ...` (or the header itself) would leave a
# manifest cargo cannot read, and the release would only fail much later.
grep -q '^\[package\]$' "$repo/dracon-system/Cargo.toml" || fail "the version rewrite dropped the [package] header"
test "$(awk -F'"' '/^\[/{p=($0=="[package]");next} p && /^version[[:space:]]*=/{print $2;exit}' "$repo/dracon-system/Cargo.toml")" = 0.1.0 \
    || fail "the [package] version was not rewritten to 0.1.0"

# --- new gate: monotonicity (audit 2026-10-01) ------------------------------
# A version that is not newer than the current one must be refused outright.
# This runs while the tree is still clean: the dry-run below deliberately
# mutates local release surfaces, and the dirty-tree check would then fire
# first and mask the gate under test.
mono_output="$work/mono.out"
current_capture="$mono_output"
if DRACON_FIXTURE_ROOT="$repo" HOME="$work/home" PATH="$work/bin:$PATH" \
    timeout 180 "$repo/dracon-system/scripts/release.sh" 0.0.0 --remote origin --yes \
    >"$mono_output" 2>&1; then
    fail "release.sh accepted a downgrade (0.0.0 after 0.1.0)"
fi
assert_contains 'is not newer than the current'

# --- folded in from the deleted test_release_standalone.sh (DECIDED 2026-10-01)
# The standalone fixture's unique assertions were the lock sync and the
# dry-run surface message; they belonged here, in the suite that actually runs.
# A second release needs notes again first: closing 0.1.0 leaves [Unreleased]
# empty, and the new gate then refuses to close an empty section — which is
# exactly what this fixture is asserting.
awk '1; /^## \[Unreleased\]$/ && !done { print ""; print "### Added"; print ""; print "- second fixture note"; done = 1 }' \
    "$repo/dracon-system/CHANGELOG.md" > "$repo/dracon-system/CHANGELOG.md.new" \
    && mv "$repo/dracon-system/CHANGELOG.md.new" "$repo/dracon-system/CHANGELOG.md"
# The tree must be clean to release, so the note is a commit of its own.
git -C "$repo" add -A
git -C "$repo" commit -qm 'fixture: add a second release note'
dry_output="$work/dry.out"
current_capture="$dry_output"
DRACON_FIXTURE_ROOT="$repo" HOME="$work/home" PATH="$work/bin:$PATH" \
    timeout 180 "$repo/dracon-system/scripts/release.sh" 0.1.1 --remote origin --dry-run --yes \
    >"$dry_output" 2>&1
assert_contains 'Cargo.lock synchronized'
assert_contains 'Local release surfaces were modified'
test -f "$repo/dracon-system/release-notes-v0.1.1.md"
# R4-M-03 follow-up: the fixture remotes are local paths, so GH_PATH must
# fall back to the documented repo — never a mangled local path — with
# the full previous-TAG...${TAG} compare range (the fixture tagged 0.1.0
# first, so this exercises the real previous-tag path, not the fallback)
# and the utility-root unit URL.
notes="$repo/dracon-system/release-notes-v0.1.1.md"
grep -F 'https://github.com/DraconDev/dracon-system-disk-process-guard-doctor/compare/dracon-system-v0.1.0...dracon-system-v0.1.1' "$notes" >/dev/null \
    || fail "compare link must use the fallback repo path and full TAG range"
grep -F 'https://raw.githubusercontent.com/DraconDev/dracon-system-disk-process-guard-doctor/main/dracon-system-guard.service' "$notes" >/dev/null \
    || fail "unit curl must hit the utility repo root"
if grep -F "$work" "$notes" >/dev/null; then
    fail "GH_PATH leaked a fixture-local remote path into release links"
fi

# --- new gate: an empty [Unreleased] must be refused, not closed -------------
# Asserted against the gate's own predicate with a fixture CHANGELOG, because
# driving it through release.sh would need a clean tree at a second version.
empty_changelog="$work/empty-changelog.md"
printf '# Changelog\n\n## [Unreleased]\n\n## [0.1.1] - 2026-01-01\n' > "$empty_changelog"
if ! awk '
    /^## \[Unreleased\]/ { seen = 1; next }
    /^## \[/ { seen = 0 }
    seen && /^[^[:space:]#]/ { found = 1 }
    END { exit !found }
' "$empty_changelog"; then
    : # the gate's own predicate: this CHANGELOG has no content to ship
else
    fail "empty-[Unreleased] fixture no longer exercises the gate"
fi

# --- auto-detect the github remote (audit R4-M-10) ---------------------------
# The github remote is deliberately NOT named `origin` (and no `origin`
# exists at all): the pre-fix REMOTE=origin default died here with
# "fatal: 'origin' does not appear to be a git repository". A
# url.insteadOf rewrite lands the real push in a local bare repo, so
# this is end-to-end with no network.
repo2="$work/repo2"
mkdir -p "$repo2/dracon-system/scripts"
git init -q -b main "$repo2"
git -C "$repo2" config core.hooksPath /dev/null
git -C "$repo2" config user.name fixture
git -C "$repo2" config user.email fixture@example.test
git init -q --bare "$work/gh.git"
git -C "$repo2" remote add upstream https://github.com/DraconDev/dracon-system-disk-process-guard-doctor.git
git -C "$repo2" config url."$work/gh.git".insteadOf https://github.com/DraconDev/dracon-system-disk-process-guard-doctor.git
cp "$SCRIPT_DIR/release.sh" "$repo2/dracon-system/scripts/release.sh"
cp "$SCRIPT_DIR/close-changelog.py" "$repo2/dracon-system/scripts/close-changelog.py"
cp "$SCRIPT_DIR/verify-install.sh" "$repo2/dracon-system/scripts/verify-install.sh"
cp "$SCRIPT_DIR/resolve-github-remote.sh" "$repo2/dracon-system/scripts/resolve-github-remote.sh"
chmod +x "$repo2/dracon-system/scripts"/*
cat > "$repo2/.gitignore" <<'EOF'
target/
dracon-system/
.publish-count
.gh-release
EOF
cat > "$repo2/Cargo.toml" <<'EOF'
[workspace]
members = ["dracon-system"]
resolver = "2"
EOF
cat > "$repo2/dracon-system/Cargo.toml" <<'EOF'
[package]
name = "dracon-system"
version = "0.0.0"
edition = "2021"
EOF
cat > "$repo2/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "dracon-system"
version = "0.0.0"
EOF
cat > "$repo2/dracon-system/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

- fixture
EOF
git -C "$repo2" add .
git -C "$repo2" add -f -- dracon-system
git -C "$repo2" commit -qm init
auto_output="$work/auto.out"
current_capture="$auto_output"
DRACON_FIXTURE_ROOT="$repo2" HOME="$work/home" PATH="$work/bin:$PATH" \
    timeout 180 "$repo2/dracon-system/scripts/release.sh" 0.1.0 --yes \
    >"$auto_output" 2>&1 || fail "auto-detect release failed"
assert_contains 'push remote: upstream (auto-detected from remote.*.url)'
assert_contains '✓ dracon-system v0.1.0 released'
git --git-dir="$work/gh.git" rev-parse refs/heads/main >/dev/null \
    || fail "auto-detected push did not land main in gh.git"
git --git-dir="$work/gh.git" rev-parse refs/tags/dracon-system-v0.1.0 >/dev/null \
    || fail "auto-detected push did not land the tag in gh.git"

echo "release pipeline regression tests: ok"
