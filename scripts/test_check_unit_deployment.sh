#!/usr/bin/env bash
# Regression tests for check-unit-deployment.sh's drift detection.
#
# Each case drives the script with explicit repo/deployed paths so the host's
# real unit is never touched, and each negative case asserts a non-zero exit —
# the guard is worthless if it passes on drift.
set -euo pipefail

SCRIPT_UNDER_TEST="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-unit-deployment.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-unit-check-XXXXXX")
trap 'rm -rf "$work"' EXIT

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

repo="$work/repo.service"
deployed="$work/deployed.service"
cat > "$repo" <<'EOF'
[Unit]
Description=fixture
[Service]
Type=simple
# /bin/sh, not /bin/true: the script runs systemd-analyze verify, and on NixOS
# /bin holds only sh — the same reason the shipped unit uses it for ExecReload.
ExecStart=/bin/sh -c 'sleep 1'
ExecReload=/bin/sh -c 'kill -HUP $MAINPID'
EOF

# 1. Identical copies are in sync.
cp "$repo" "$deployed"
"$SCRIPT_UNDER_TEST" "$repo" "$deployed" >/dev/null || fail "identical units reported stale"

# 2. The deployed copy missing ExecReload is exactly the 2026-09-29 bug: the
#    reload handler is shipped but not deployed. Must fail with a redeploy hint.
grep -v '^ExecReload=' "$repo" > "$deployed"
if out="$("$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)"; then
    fail "a deployed unit missing ExecReload was reported as in sync"
fi
grep -q 'daemon-reload' <<<"$out" || fail "stale-unit output has no redeploy command"
grep -q 'ExecReload' <<<"$out" || fail "stale-unit output does not name the divergent directive"

# 3. A host that never deployed the unit is not drift, and must not fail — the
#    release pipeline runs this on machines without a user systemd.
if out="$("$SCRIPT_UNDER_TEST" "$repo" "$work/absent.service" 2>&1)"; then
    :
else
    fail "a missing deployed unit was treated as stale: $out"
fi
grep -q 'nothing to compare' <<<"$out" || fail "missing-unit output is not self-explanatory"

# 4. A missing repo copy is a script error, not a silent pass.
if "$SCRIPT_UNDER_TEST" "$work/absent.service" "$deployed" >/dev/null 2>&1; then
    fail "a missing repo unit copy was reported as clean"
fi

# 5. Comment-only differences still count as divergence: the deployed file must
#    be the shipped file, byte for byte, so an operator's local edit cannot
#    quietly shadow a shipped directive.
printf '# local operator note\n' >> "$deployed"
if "$SCRIPT_UNDER_TEST" "$repo" "$deployed" >/dev/null 2>&1; then
    fail "a locally edited deployed unit was reported as in sync"
fi

echo "check-unit-deployment regression tests: ok"
