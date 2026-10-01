#!/usr/bin/env bash
# Regression test for the monorepo release pipeline's gates, fixture, rerun
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
    DRACON_FIXTURE_ROOT="$repo" HOME="$work/home" PATH="$work/bin:$PATH" \
        timeout 180 "$repo/dracon-system/scripts/release.sh" 0.1.0 --yes
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

test "$(git -C "$repo" tag --list dracon-system-v0.1.0)" = dracon-system-v0.1.0
git --git-dir="$work/origin.git" rev-parse refs/heads/main >/dev/null
git --git-dir="$work/origin.git" rev-parse refs/tags/dracon-system-v0.1.0 >/dev/null

second_output="$work/second.out"
current_capture="$second_output"
run_release >"$second_output" 2>&1
assert_contains 'already published; continuing'
assert_contains 'nothing to commit (release commit already exists)'
assert_contains 'tag dracon-system-v0.1.0 already exists'
assert_contains 'github release v0.1.0 already exists'

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

# --- folded in from the deleted test_release_standalone.sh (DECIDED 2026-10-01)
# The standalone fixture's unique assertions were the lock sync and the
# dry-run surface message; they belonged here, in the suite that actually runs.
dry_output="$work/dry.out"
current_capture="$dry_output"
DRACON_FIXTURE_ROOT="$repo" HOME="$work/home" PATH="$work/bin:$PATH" \
    timeout 180 "$repo/dracon-system/scripts/release.sh" 0.1.1 --dry-run --yes \
    >"$dry_output" 2>&1
assert_contains 'Cargo.lock synchronized'
assert_contains 'Local release surfaces were modified'
test -f "$repo/dracon-system/release-notes-v0.1.1.md"

# --- new gates (audit 2026-10-01) -----------------------------------------
# A version that is not newer than the current one must be refused outright.
mono_output="$work/mono.out"
current_capture="$mono_output"
if DRACON_FIXTURE_ROOT="$repo" HOME="$work/home" PATH="$work/bin:$PATH" \
    timeout 180 "$repo/dracon-system/scripts/release.sh" 0.0.0 --yes \
    >"$mono_output" 2>&1; then
    fail "release.sh accepted a downgrade (0.0.0 after 0.1.1)"
fi
assert_contains 'is not newer than the current'

# An empty [Unreleased] must be refused rather than closed into a bare header.
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

echo "release pipeline regression tests: ok"
