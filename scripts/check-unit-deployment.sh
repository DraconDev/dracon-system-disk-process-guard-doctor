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
# systemd searches BOTH "$XDG_CONFIG_HOME/systemd/user" and
# "$HOME/.config/systemd/user" for user units, so this script searches both.
# Checking only one lets a drifted, deployed unit hide behind the other path and
# the script reports "nothing to compare" — a silent false pass, which is the
# exact failure this script exists to prevent. HOME may also be unset in a
# stripped container or CI environment, where `set -u` would turn "${HOME}/..."
# into a hard abort; an unset HOME is not drift either, it means there is no
# user unit directory to compare against. An explicit second argument always
# wins, which is what the regression suite passes.
if [ -n "${2:-}" ]; then
    DEPLOYED_UNIT="$2"
else
    unit_dirs=()
    [ -n "${XDG_CONFIG_HOME:-}" ] && unit_dirs+=("$XDG_CONFIG_HOME/systemd/user")
    [ -n "${HOME:-}" ] && unit_dirs+=("$HOME/.config/systemd/user")
    if [ ${#unit_dirs[@]} -eq 0 ]; then
        echo "• neither XDG_CONFIG_HOME nor HOME is set — no user unit directory to compare"
        exit 0
    fi
    # First candidate that actually holds the unit wins; with both present the
    # one systemd reports is preferred by the loaded-unit check further down.
    DEPLOYED_UNIT=""
    for dir in "${unit_dirs[@]}"; do
        # `-e || -L`, not `-f`: `-f` follows symlinks, so a unit symlinked to a
        # GC'd nix store path (or any dangling link) tested false and the script
        # reported "nothing to compare" — a silent false pass over a unit that IS
        # deployed and IS broken. A dangling link must reach the comparison so
        # it is reported (audit 2026-10-01).
        if [ -e "$dir/$UNIT_NAME" ] || [ -L "$dir/$UNIT_NAME" ]; then
            DEPLOYED_UNIT="$dir/$UNIT_NAME"
            break
        fi
    done
    if [ -z "$DEPLOYED_UNIT" ]; then
        primary="${unit_dirs[0]}/$UNIT_NAME"
        echo "• no deployed unit at $primary — nothing to compare (install it with:"
        echo "    mkdir -p \"\$(dirname \"$primary\")\" && install -m 644 \"$REPO_UNIT\" \"$primary\" && systemctl --user daemon-reload)"
        exit 0
    fi
fi
REDEPLOY_CMD="install -m 644 \"$REPO_UNIT\" \"$DEPLOYED_UNIT\" && systemctl --user daemon-reload"
# Overridable so the regression suite can drive the systemd-dependent steps
# deterministically instead of depending on the host's user manager.
SYSTEMCTL="${SYSTEMCTL:-systemctl}"
SYSTEMD_ANALYZE="${SYSTEMD_ANALYZE:-systemd-analyze}"

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
#    two files agree.
#
#    Both this step and step 5 need a running user manager, and a host without
#    one (CI, a container, a bare ssh session) has no opinion about the loaded
#    unit. Probing first keeps "cannot ask systemd" from being reported as
#    "systemd says no" — with no bus reachable, `systemctl --user show` and
#    `systemd-analyze --user verify` both exit non-zero ("Failed to connect to
#    user scope bus", "Failed to initialize manager") and would otherwise turn
#    an in-sync unit into a false alarm with a wrong remediation.
if command -v "$SYSTEMCTL" >/dev/null 2>&1 &&
    "$SYSTEMCTL" --user show -p Version --value >/dev/null 2>&1; then
    if grep -qE '^ExecReload=' "$REPO_UNIT"; then
        loaded="$("$SYSTEMCTL" --user show "$UNIT_NAME" -p ExecReload --value 2>/dev/null || true)"
        if [ -z "$loaded" ]; then
            echo "✗ systemd is running a unit without ExecReload although the shipped" >&2
            echo "  unit has one: the file was copied but never reloaded. Run:" >&2
            echo "    systemctl --user daemon-reload && systemctl --user reload $UNIT_NAME" >&2
            exit 1
        fi
    fi

    # 5. The deployed unit must be a valid unit file, so a redeploy that
    #    "succeeded" can never leave the guard unstartable. Only meaningful when
    #    there is a user manager to initialise (see step 4).
    if command -v "$SYSTEMD_ANALYZE" >/dev/null 2>&1; then
        if ! verify_out="$("$SYSTEMD_ANALYZE" --user verify "$DEPLOYED_UNIT" 2>&1)"; then
            echo "✗ systemd-analyze --user verify rejected $DEPLOYED_UNIT" >&2
            printf '%s\n' "$verify_out" | sed -e 's/^/  /' | head -20 >&2
            exit 1
        fi
    fi
else
    echo "• no reachable user systemd — skipped the loaded-unit and verify checks"
fi

echo "✓ OK: deployed unit matches the shipped unit ($DEPLOYED_UNIT)."
exit 0
