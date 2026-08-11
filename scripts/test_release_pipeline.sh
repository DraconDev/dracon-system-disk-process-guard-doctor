#!/usr/bin/env bash
# Regression test for the release pipeline's gates, fixture, rerun branches,
# and mirror-tag reminder. All external release commands are local stubs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-release-pipeline-XXXXXX")
trap 'rm -rf "$work"' EXIT

repo="$work/repo"
mkdir -p "$repo/scripts" "$work/bin" "$work/home/.cargo"
git init -q -b main "$repo"
git -C "$repo" config core.hooksPath /dev/null
git -C "$repo" config user.name fixture
git -C "$repo" config user.email fixture@example.test

git init -q --bare "$work/origin.git"
git init -q --bare "$work/gitlab.git"
git -C "$repo" remote add origin "$work/origin.git"
git -C "$repo" remote add gitlab "$work/gitlab.git"

cp "$SCRIPT_DIR/release.sh" "$repo/scripts/release.sh"
cp "$SCRIPT_DIR/close-changelog.py" "$repo/scripts/close-changelog.py"
cp "$SCRIPT_DIR/verify-install.sh" "$repo/scripts/verify-install.sh"
chmod +x "$repo/scripts"/*.sh "$repo/scripts"/*.py
cat > "$repo/.gitignore" <<'EOF'
target/
.publish-count
.gh-release
EOF
cat > "$repo/Cargo.toml" <<'EOF'
[package]
name = "dracon-system"
version = "0.0.0"
edition = "2021"
EOF
cat > "$repo/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

- fixture
EOF

git -C "$repo" add .
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
    test|build|clippy)
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
        timeout 180 "$repo/scripts/release.sh" 0.1.0 --yes
}

first_output="$work/first.out"
run_release >"$first_output" 2>"$work/first.err"
grep -F 'all gates passed' "$first_output" >/dev/null
grep -F 'fixture check on packaged artifact' "$first_output" >/dev/null
grep -F 'mirror remotes get main from the daemon' "$first_output" >/dev/null
grep -F 'git push gitlab v0.1.0' "$first_output" >/dev/null
grep -F '✓ dracon-system v0.1.0 released' "$first_output" >/dev/null

test "$(git -C "$repo" tag --list v0.1.0)" = v0.1.0
git --git-dir="$work/origin.git" rev-parse refs/heads/main >/dev/null
git --git-dir="$work/origin.git" rev-parse refs/tags/v0.1.0 >/dev/null

second_output="$work/second.out"
run_release >"$second_output" 2>"$work/second.err"
grep -F 'already published; continuing' "$second_output" >/dev/null
grep -F 'nothing to commit (release commit already exists)' "$second_output" >/dev/null
grep -F 'tag v0.1.0 already exists' "$second_output" >/dev/null
grep -F 'github release v0.1.0 already exists' "$second_output" >/dev/null

test "$(cat "$repo/.publish-count")" = 2
test "$(git -C "$repo" log --format=%s -1)" = 'release: v0.1.0'
test "$(git -C "$repo" status --porcelain)" = ''

echo "release pipeline regression tests: ok"
