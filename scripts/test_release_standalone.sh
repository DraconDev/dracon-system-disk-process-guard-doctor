#!/usr/bin/env bash
# Verify that the monorepo, rather than a utility subdirectory treated as a
# standalone repository, can execute the system release gates and preview.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-release-monorepo-XXXXXX")
trap 'rm -rf "$work"' EXIT
host_cargo_home="${CARGO_HOME:-$HOME/.cargo}"
host_rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"

if [[ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]]; then
    echo "monorepo release regression requires a clean source tree" >&2
    exit 1
fi

git clone --no-local --quiet "$REPO_ROOT" "$work/repo"
clone="$work/repo"
test -f "$clone/Cargo.lock"
test -f "$clone/deny.toml"
test -f "$clone/dracon-system/Cargo.toml"
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

echo 'monorepo release gates: ok'

# Exercise the documented local preview and rollback contract against the real
# release script. This catches a bumped package whose omitted workspace
# lockfile would make the next locked preflight fail.
current_version=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' dracon-system/Cargo.toml)
base_version="${current_version%%-*}"
IFS=. read -r major minor patch <<< "$base_version"
next_version="${major}.${minor}.$((patch + 1))"
baseline_commit=$(git rev-parse HEAD)
baseline_toml_sha=$(sha256sum dracon-system/Cargo.toml | awk '{print $1}')
baseline_lock_sha=$(sha256sum Cargo.lock | awk '{print $1}')
baseline_changelog_sha=$(sha256sum dracon-system/CHANGELOG.md | awk '{print $1}')
mkdir -p "$work/bin"
cat > "$work/bin/gh" <<'GH'
#!/usr/bin/env bash
[[ "${1:-}" == auth && "${2:-}" == status ]]
GH
chmod +x "$work/bin/gh"

preview_out="$work/dry-run.out"
# The dry-run exercises `cargo test --workspace --locked`, which for
# dracon-system includes dracon-security tests that require master
# identities from the real HOME (see lib.rs load_master_identities).
# Preserve the operator HOME for that step; gh is stubbed via PATH
# and cargo credentials are already present in the real HOME.
CARGO_HOME="$host_cargo_home" RUSTUP_HOME="$host_rustup_home" \
PATH="$work/bin:$PATH" CARGO_TARGET_DIR="$work/target" \
timeout 1200 "$clone/dracon-system/scripts/release.sh" "$next_version" --dry-run \
    >"$preview_out" 2>"$work/dry-run.err"
grep -F 'Cargo.lock synchronized' "$preview_out" >/dev/null
grep -F 'Local release surfaces were modified' "$preview_out" >/dev/null
test "$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' dracon-system/Cargo.toml)" = "$next_version"
grep -q "^## \[$next_version\] - " dracon-system/CHANGELOG.md
test -f "dracon-system/release-notes-v${next_version}.md"
grep -A2 'name = "dracon-system"' Cargo.lock \
    | grep -q "version = \"$next_version\""
changed_paths=$(git status --porcelain | awk '{print $2}' | sort)
expected_paths=$(printf 'Cargo.lock\ndracon-system/CHANGELOG.md\ndracon-system/Cargo.toml\n' | sort | tr -d '\r')
# Normalize: both are sorted newline-joined lists; compare sorted.
test "$changed_paths" = "$expected_paths"

# The updated workspace lockfile must make the post-bump locked preflight
# succeed.
run_gate post-test cargo test --workspace --locked

CARGO_HOME="$host_cargo_home" RUSTUP_HOME="$host_rustup_home" \
PATH="$work/bin:$PATH" CARGO_TARGET_DIR="$work/target" \
timeout 120 "$clone/dracon-system/scripts/release.sh" --abort \
    >"$work/abort.out" 2>"$work/abort.err"
test "$(git rev-parse HEAD)" = "$baseline_commit"
test "$(sha256sum dracon-system/Cargo.toml | awk '{print $1}')" = "$baseline_toml_sha"
test "$(sha256sum Cargo.lock | awk '{print $1}')" = "$baseline_lock_sha"
test "$(sha256sum dracon-system/CHANGELOG.md | awk '{print $1}')" = "$baseline_changelog_sha"
test ! -e "dracon-system/release-notes-v${next_version}.md"
test -z "$(git status --porcelain)"
echo 'monorepo dry-run/abort: ok'
