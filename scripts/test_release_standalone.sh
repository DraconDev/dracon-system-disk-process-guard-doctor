#!/usr/bin/env bash
# Verify that the standalone repository, rather than the parent workspace,
# can execute every release gate from a clean clone.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-release-standalone-XXXXXX")
trap 'rm -rf "$work"' EXIT
host_cargo_home="${CARGO_HOME:-$HOME/.cargo}"
host_rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"

if [[ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]]; then
    echo "standalone release regression requires a clean source tree" >&2
    exit 1
fi

git clone --no-local --quiet "$REPO_ROOT" "$work/repo"
clone="$work/repo"
test -f "$clone/Cargo.lock"
test -f "$clone/deny.toml"
test -z "$(git -C "$clone" status --porcelain)"

run_gate() {
    local name=$1
    shift
    local log="$work/${name}.log"
    printf '  %-8s' "$name"
    if CARGO_TARGET_DIR="$work/target" CARGO_TERM_COLOR=never \
        timeout 420 "$@" >"$log" 2>&1; then
        echo 'ok'
    else
        local rc=$?
        echo "FAILED (exit $rc)" >&2
        tail -80 "$log" >&2
        return "$rc"
    fi
}

cd "$clone"
run_gate metadata cargo metadata --format-version 1 --locked --no-deps
run_gate test cargo test --workspace --locked
run_gate build cargo build --release --locked
run_gate deny cargo deny check
run_gate clippy cargo clippy --workspace --locked -- -D warnings

echo 'standalone release gates: ok'

# Exercise the documented local preview and rollback contract against the real
# release script. This catches a bumped manifest whose omitted lockfile would
# make the next locked preflight fail.
next_version=0.112.37
baseline_commit=$(git rev-parse HEAD)
baseline_toml_sha=$(sha256sum Cargo.toml | awk '{print $1}')
baseline_lock_sha=$(sha256sum Cargo.lock | awk '{print $1}')
baseline_changelog_sha=$(sha256sum CHANGELOG.md | awk '{print $1}')
mkdir -p "$work/bin" "$work/home/.cargo"
cat > "$work/bin/gh" <<'GH'
#!/usr/bin/env bash
[[ "${1:-}" == auth && "${2:-}" == status ]]
GH
chmod +x "$work/bin/gh"
touch "$work/home/.cargo/credentials.toml"

preview_out="$work/dry-run.out"
HOME="$work/home" CARGO_HOME="$host_cargo_home" RUSTUP_HOME="$host_rustup_home" \
PATH="$work/bin:$PATH" CARGO_TARGET_DIR="$work/target" \
timeout 1200 "$clone/scripts/release.sh" "$next_version" --dry-run \
    >"$preview_out" 2>"$work/dry-run.err"
grep -F 'Cargo.lock synchronized' "$preview_out" >/dev/null
grep -F 'Local release surfaces were modified' "$preview_out" >/dev/null
test "$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)" = "$next_version"
grep -q "^## \[$next_version\] - " CHANGELOG.md
test -f "release-notes-v${next_version}.md"
grep -A2 'name = "dracon-system"' Cargo.lock \
    | grep -q "version = \"$next_version\""
changed_paths=$(git status --porcelain | awk '{print $2}' | sort)
test "$changed_paths" = $'CHANGELOG.md\nCargo.lock\nCargo.toml\nrelease-notes-v0.112.37.md'

# The regenerated lockfile must make the post-bump locked preflight succeed.
run_gate post-test cargo test --workspace --locked

HOME="$work/home" CARGO_HOME="$host_cargo_home" RUSTUP_HOME="$host_rustup_home" \
PATH="$work/bin:$PATH" CARGO_TARGET_DIR="$work/target" \
timeout 120 "$clone/scripts/release.sh" "$next_version" --abort \
    >"$work/abort.out" 2>"$work/abort.err"
test "$(git rev-parse HEAD)" = "$baseline_commit"
test "$(sha256sum Cargo.toml | awk '{print $1}')" = "$baseline_toml_sha"
test "$(sha256sum Cargo.lock | awk '{print $1}')" = "$baseline_lock_sha"
test "$(sha256sum CHANGELOG.md | awk '{print $1}')" = "$baseline_changelog_sha"
test ! -e "release-notes-v${next_version}.md"
test -z "$(git status --porcelain)"
echo 'standalone dry-run/abort: ok'
