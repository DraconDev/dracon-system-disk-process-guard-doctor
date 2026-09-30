#!/usr/bin/env bash
# scripts/check-unit-deployment.sh — assert that the deployed systemd user unit
# still matches the unit shipped in this repo.
#
# A unit change that lands in the repo but is never copied to
# ~/.config/systemd/user silently keeps the live service on the old unit. That
# has now shipped twice: audit F94 (2026-09-27) widened ReadWritePaths so
# log truncation could reach ~/.local/share, and ExecReload (2026-09-29) made
# `systemctl --user reload` work — both sat in the repo while the live unit
# stayed at the 2026-09-20 copy, so the deployed service could neither
# truncate its logs nor be reloaded. A release cannot detect that on its own
# (deployment state belongs to the host), so this check makes the drift
# explicit instead of leaving it to be found by a failing command.
#
# Usage: scripts/check-unit-deployment.sh [repo-unit] [deployed-unit]
# Exit codes: 0 = in sync, not deployed, or nothing checkable here;
#             1 = the deployed unit is stale, unverified, or systemd has not
#                 picked the file up.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNIT_NAME="dracon-system-guard.service"
REPO_UNIT="${1:-$SCRIPT_DIR/../$UNIT_NAME}"
DEPLOYED_UNIT="${2:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$UNIT_NAME}"
REDEPLOY_CMD="install -m 644 $REPO_UNIT $DEPLOYED_UNIT && systemctl --user daemon-reload"

stale() {
    echo "✗ deployed unit is STALE: $DEPLOYED_UNIT" >&2
    echo "  the shipped unit in this repo differs from the deployed copy" >&2
    diff -u "$DEPLOYED_UNIT" "$REPO_UNIT" 2>/dev/null | sed -e 's/^/  /' | head -40 >&2 || true
    echo "  redeploy with:" >&2
    echo "    $REDEPLOY_CMD" >&2
    exit 1
}

# 1. Repo copy must exist — without it there is nothing to compare against.
if [ ! -f "$REPO_UNIT" ]; then
    echo "✗ repo unit copy not found: $REPO_UNIT" >&2
    exit 1
fi

# 2. A host that never installed the unit has no drift to report (containers,
#    CI, fresh clones). Silence with a one-line note, not a failure.
if [ ! -f "$DEPLOYED_UNIT" ]; then
    echo "• no deployed unit at $DEPLOYED_UNIT — nothing to compare (install it with:"
    echo "    mkdir -p \$(dirname \"$DEPLOYED_UNIT\") && $REDEPLOY_CMD)"
    exit 0
fi

# 3. The core invariant: the deployed file is the shipped file.
if ! cmp -s "$REPO_UNIT" "$DEPLOYED_UNIT"; then
    stale
fi

# 4. The deployed file is the one systemd is running. If it is not, the file was
#    copied without `daemon-reload` and the drift is still live even though the
#    two files agree. Best-effort: hosts without a user manager report nothing.
if command -v systemctl >/dev/null 2>&1; then
    loaded="$(systemctl --user show "$UNIT_NAME" -p ExecReload --value 2>/dev/null || true)"
    repo_has_reload=no
    grep -qE '^ExecReload=' "$REPO_UNIT" && repo_has_reload=yes
    if [ "$repo_has_reload" = yes ] && [ -z "$loaded" ]; then
        echo "✗ systemd is running a unit without ExecReload although the shipped" >&2
        echo "  unit has one: the file was copied but never reloaded. Run:" >&2
        echo "    systemctl --user daemon-reload && systemctl --user reload $UNIT_NAME" >&2
        exit 1
    fi
fi

# 5. The deployed unit must be a valid unit file, so a redeploy that "succeeded"
#    can never leave the guard unstartable.
if command -v systemd-analyze >/dev/null 2>&1; then
    if ! verify_out="$(systemd-analyze --user verify "$DEPLOYED_UNIT" 2>&1)"; then
        echo "✗ systemd-analyze --user verify rejected $DEPLOYED_UNIT" >&2
        printf '%s\n' "$verify_out" | sed -e 's/^/  /' | head -20 >&2
        exit 1
    fi
fi

echo "✓ OK: deployed unit matches the shipped unit ($DEPLOYED_UNIT)."
exit 0
