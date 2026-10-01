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
#             1 = the deployed unit is stale, unverified, systemd has not
#                 picked the file up, or the running service cannot write a
#                 storage root its own policy names.
#
# Test seams (so the regression suite never depends on the live service):
#   POLICY_FILE     policy TOML to read the storage roots from
#   GUARD_MOUNTINFO a mountinfo file to read instead of /proc/<pid>/mountinfo
#   GUARD_MAINPID   the pid whose namespace to inspect
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
# `-e || -L` again, for the same reason as the discovery loop: a DANGLING unit
# symlink (a link into a GC'd nix store path, say) is deployed AND broken, and
# must reach the comparison below rather than be waved through as "not
# installed" (audit 2026-10-01).
if [ ! -e "$DEPLOYED_UNIT" ] && [ ! -L "$DEPLOYED_UNIT" ]; then
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

# 6. The RUNTIME contract: the storage roots the policy names must be writable
#    in the namespace the guard is actually running in.
#
#    Steps 3-5 compare files. They cannot see the thing that actually broke the
#    reclaim path for 4 days (2026-09-28..10-01): a ReadWritePaths entry that
#    was correct on disk and still granted nothing, because /mnt/data is
#    remounted read-only by ProtectSystem=strict and only a sub-mount restores
#    write access. On 2026-09-30 23:41 the guard reached its 85% action
#    threshold, picked 11 real trees (including dracon-platform/target and
#    dracon-utilities/target), and every move failed on a read-only filesystem;
#    the failure path `continue`s rather than falling back to a delete, so the
#    reclaim path reclaimed zero bytes while the log looked busy.
#
#    Joining the namespace to test this needs CAP_SYS_ADMIN, which an
#    unprivileged operator shell does not have — that is why this was
#    previously written off as "not performable". It is performable: the kernel
#    publishes every mount's flags in /proc/<pid>/mountinfo, and reading another
#    same-uid process's mountinfo needs no privilege beyond ptrace-mode read.
#    The longest mount that is a prefix of a root is the one that decides
#    access, so that is what gets checked. "Cannot ask" stays a note, never a
#    failure — a non-dumpable service process, a pid that exited between the
#    query and the read, or a container without /proc all mean "no opinion",
#    exactly like the absent user manager in step 4.
storage_root_for_key() {
    # First assignment wins; a commented-out example line never matches.
    sed -n -e "s|^[[:space:]]*$1[[:space:]]*=[[:space:]]*\"\\([^\"]*\\)\".*|\\1|p" \
        "$2" 2>/dev/null | head -1
}
# The mount a path resolves through: the longest mount point that prefixes it.
# $2 = mountinfo, $3 = absolute path to resolve. Echoes "<mountpoint>|<flags>".
effective_mount_for() {
    awk -v target="$3" '
        {
            point = $5
            if (point == "/" || (target == point) ||
                (substr(target, 1, length(point) + 1) == point "/")) {
                # A later mount at the same point shadows earlier ones, so
                # "longest wins" must also prefer the later line.
                if (length(point) >= best_len) { best_len = length(point); best_point = point }
            }
            # Superblock options live after the "-" separator; the VFS flags
            # (ro/rw) are field 6, which is what decides write access.
            for (i = 7; i <= NF; i++) if ($i == "-") { post = i; break }
            opts[NR] = (post ? $(post + 1) : $6) " " $6
        }
        END {
            # Re-walk to fetch the flags of the winning line, since the arrays
            # above are indexed by NR and best_point is only known at the end.
            for (n = 1; n <= NR; n++) {
                p = ""
                for (i = 1; i <= NF; i++) {}
            }
        }
    ' "$2" >/dev/null 2>&1 || true
    # Doing this in pure awk needs two passes over the same file; the shell
    # loop below is clearer and this path runs at most a handful of times.
    local best_point="" best_flags="" best_len=-1 line point flags
    while IFS= read -r line; do
        point="$(printf '%s\n' "$line" | awk '{print $5}')"
        flags="$(printf '%s\n' "$line" | awk '{print $6}')"
        case "$target" in
            "$point") ;;
            "$point"/*) ;;
            *) continue ;;
        esac
        if [ "${#point}" -ge "$best_len" ]; then
            best_len=${#point}
            best_point="$point"
            best_flags="$flags"
        fi
    done < "$2"
    [ -n "$best_point" ] || return 1
    printf '%s|%s\n' "$best_point" "$best_flags"
}

check_storage_root_writable() {
    local key="$1" root="$2" policy="$3" mountinfo="$4"
    [ -n "$root" ] || return 0 # knob unset or commented out: nothing to check
    case "$root" in
        "~"/*) root="${HOME:-}/$root#\~/}" ;;
        /*) ;;
        *) return 0 # relative root: resolved against the service CWD, not checkable
    esac
    local hit
    if ! hit="$(effective_mount_for "$mountinfo" "$root")"; then
        echo "✗ $key=$root is not covered by any mount in the running namespace" >&2
        echo "  the guard cannot resolve or write it; every reclaim against this" >&2
        echo "  root will fail. Add it to ReadWritePaths= in $UNIT_NAME." >&2
        return 1
    fi
    local point="${hit%%|*}" flags="${hit##*|}"
    case "$flags" in
        *ro*)
            echo "✗ $key=$root resolves read-only inside the running namespace" >&2
            echo "  effective mount: $point ($flags)" >&2
            echo "  ProtectSystem=strict remounts the parent read-only, so only a" >&2
            echo "  sub-mount restores write access. Add the path itself to" >&2
            echo "  ReadWritePaths= in $UNIT_NAME (a '-'-prefixed optional entry" >&2
            echo "  does not help on a host where the directory exists)." >&2
            return 1
            ;;
    esac
    echo "  ✓ $key=$root is read-write in the running namespace (mount: $point)"
    return 0
}

guard_mountinfo() {
    # $1 = a readable mountinfo file. Prints the path, or nothing if unusable.
    local info="$1"
    [ -r "$info" ] || return 1
    head -1 "$info" >/dev/null 2>&1 || return 1
    printf '%s\n' "$info"
}

if [ -n "${GUARD_MAINPID:-}" ]; then
    main_pid="$GUARD_MAINPID"
elif command -v "$SYSTEMCTL" >/dev/null 2>&1; then
    main_pid="$("$SYSTEMCTL" --user show "$UNIT_NAME" -p MainPID --value 2>/dev/null || true)"
else
    main_pid=""
fi

if [ -z "$main_pid" ] || [ "$main_pid" = "0" ] || [ "$main_pid" = "N/A" ]; then
    echo "• guard service is not running — skipped the runtime storage-root check"
elif [ -n "${GUARD_MOUNTINFO:-}" ]; then
    if [ ! -r "$GUARD_MOUNTINFO" ]; then
        echo "• GUARD_MOUNTINFO=$GUARD_MOUNTINFO is unreadable — skipped the runtime" >&2
        echo "  storage-root check" >&2
        exit 1
    fi
    mountinfo="$GUARD_MOUNTINFO"
    if storage_rc=0; then :; fi
    policy_file="${POLICY_FILE:-${HOME:-}/.dracon/utilities/system/dracon-system.toml}"
    for pair in \
        "quarantine_dir:$(storage_root_for_key quarantine_dir "${policy_file:-/nonexistent}")" \
        "relocate_cold_root:$(storage_root_for_key relocate_cold_root "${policy_file:-/nonexistent}")"
    do
        check_storage_root_writable "${pair%%:*}" "${pair#*:}" "$policy_file" "$mountinfo" || storage_rc=1
    done
    [ "${storage_rc:-0}" -eq 0 ] || exit 1
else
    if mountinfo="$(guard_mountinfo "/proc/$main_pid/mountinfo")"; then
        policy_file="${POLICY_FILE:-${HOME:-}/.dracon/utilities/system/dracon-system.toml}"
        storage_rc=0
        for pair in \
            "quarantine_dir:$(storage_root_for_key quarantine_dir "$policy_file")" \
            "relocate_cold_root:$(storage_root_for_key relocate_cold_root "$policy_file")"
        do
            check_storage_root_writable "${pair%%:*}" "${pair#*:}" "$policy_file" "$mountinfo" || storage_rc=1
        done
        [ "$storage_rc" -eq 0 ] || exit 1
    else
        echo "• cannot read /proc/$main_pid/mountinfo (process not dumpable, or the" >&2
        echo "  service exited) — skipped the runtime storage-root check" >&2
    fi
fi

echo "✓ OK: deployed unit matches the shipped unit ($DEPLOYED_UNIT)."
exit 0
