#!/usr/bin/env bash
# Verify that the standalone repository, rather than the parent workspace,
# can execute every release gate from a clean clone.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-release-standalone-XXXXXX")
trap 'rm -rf "$work"' EXIT

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
