#!/usr/bin/env bash
# Regression tests for check-unit-deployment.sh's drift detection.
#
# Every case drives the script with explicit repo/deployed paths so the host's
# real unit is never touched, and stubs the systemd-facing commands so the
# loaded-unit and verify steps are exercised deterministically instead of
# depending on whatever user manager the machine running the suite happens to
# have. Every negative case asserts a non-zero exit — the guard is worthless if
# it passes on drift.
set -euo pipefail

SCRIPT_UNDER_TEST="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-unit-deployment.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-unit-check-XXXXXX")
trap 'rm -rf "$work"' EXIT

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

# --- systemd stubs -----------------------------------------------------------
# $1: what the loaded unit reports for -p ExecReload ("present", "empty", or
# "unreachable" for a host with no user manager at all).
make_systemctl() {
    local mode="$1" stub="$work/bin/systemctl-$1"
    mkdir -p "$work/bin"
    cat > "$stub" <<EOF
#!/usr/bin/env bash
# Fixture for mode: $mode
for arg in "\$@"; do
    case "\$arg" in
        -p)
            next_is_property=1
            continue
            ;;
    esac
    if [ "\${next_is_property:-0}" = 1 ]; then
        next_is_property=0
        property="\$arg"
    fi
done
if [ "\${property:-}" = Version ]; then
    case "$mode" in
        unreachable) exit 1 ;;
        *) printf '%s\n' 258.7; exit 0 ;;
    esac
fi
if [ "\${property:-}" = ExecReload ]; then
    case "$mode" in
        unreachable) exit 1 ;;
        empty) exit 0 ;;
        *) printf '%s\n' "{ path=/bin/sh ; argv[]=/bin/sh -c kill -HUP \$MAINPID ; status=0/0 }"; exit 0 ;;
    esac
fi
# Any other query: answer as a reachable manager would.
case "$mode" in
    unreachable) exit 1 ;;
    *) exit 0 ;;
esac
EOF
    chmod +x "$stub"
    printf '%s' "$stub"
}

# systemd-analyze stubs. Every case that stubs SYSTEMCTL also pins one of these,
# so the suite never inherits the invoking shell's real systemd-analyze: on a
# host with no user manager the real one exits 1 ("Failed to initialize
# manager"), which would make a clean case fail for a reason that has nothing to
# do with the condition under test — and would make a case expecting failure
# pass for the wrong reason.
analyze_clean="$work/bin/systemd-analyze-clean"
analyze_rejects="$work/bin/systemd-analyze-rejects"
mkdir -p "$work/bin"
cat > "$analyze_clean" <<'EOF'
#!/usr/bin/env bash
# Fixture: the unit verifies clean.
exit 0
EOF
cat > "$analyze_rejects" <<'EOF'
#!/usr/bin/env bash
echo "fixture.service: Command /nonexistent is not executable: No such file or directory" >&2
exit 1
EOF
chmod +x "$analyze_clean" "$analyze_rejects"

# --- fixtures ----------------------------------------------------------------
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

# 1. Identical copies, and the loaded unit really carries ExecReload: in sync.
cp "$repo" "$deployed"
out="$(SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" ||
    fail "identical units reported stale: $out"

# 2. The files agree but systemd is running a unit without ExecReload — the file
#    was copied and `daemon-reload` was forgotten, so the drift is still live.
#    This is the step the earlier version of this suite never reached.
out="$(SYSTEMCTL="$(make_systemctl empty)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" &&
    fail "a copied-but-not-reloaded unit was reported as in sync"
grep -q 'daemon-reload' <<<"$out" || fail "no redeploy hint: $out"
grep -q 'ExecReload' <<<"$out" || fail "the divergent directive is not named: $out"

# 3. A host with no reachable user manager (CI, container, bare ssh) has no
#    opinion about the loaded unit. "Cannot ask systemd" must never be reported
#    as "systemd says no" — that is a false alarm with a wrong remediation.
out="$(SYSTEMCTL="$(make_systemctl unreachable)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" ||
    fail "an unreachable user manager was reported as stale: $out"
grep -q 'no reachable user systemd' <<<"$out" ||
    fail "the skipped-checks note is missing: $out"

# 4. Same case against the real commands with the bus env removed, which is how
#    a container reaches the script. Must take the documented exit-0 path.
out="$(env -u DBUS_SESSION_BUS_ADDRESS -u XDG_RUNTIME_DIR \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" ||
    fail "no-bus host was reported as stale: $out"

# 5. A deployed copy missing ExecReload is the 2026-09-29 bug in the file
#    itself: the reload handler is shipped but never deployed.
grep -v '^ExecReload=' "$repo" > "$deployed"
out="$(SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" &&
    fail "a deployed unit missing ExecReload was reported as in sync"
grep -q 'daemon-reload' <<<"$out" || fail "stale-unit output has no redeploy command"

# 6. A host that never deployed the unit is not drift, and must not fail — the
#    release pipeline runs this on machines without a user systemd.
if out="$("$SCRIPT_UNDER_TEST" "$repo" "$work/absent.service" 2>&1)"; then
    :
else
    fail "a missing deployed unit was treated as stale: $out"
fi
grep -q 'nothing to compare' <<<"$out" || fail "missing-unit output is not self-explanatory"

# 7. A missing repo copy is a script error, not a silent pass.
if "$SCRIPT_UNDER_TEST" "$work/absent.service" "$deployed" >/dev/null 2>&1; then
    fail "a missing repo unit copy was reported as clean"
fi

# 8. Comment-only differences still count as divergence: the deployed file must
#    be the shipped file, byte for byte, so an operator's local edit cannot
#    quietly shadow a shipped directive.
cp "$repo" "$deployed"
printf '# local operator note\n' >> "$deployed"
if SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" >/dev/null 2>&1; then
    fail "a locally edited deployed unit was reported as in sync"
fi

# 9. systemd-analyze rejecting the deployed file is a real failure, but only
#    when there is a manager to ask.
out="$(SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_rejects" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" &&
    fail "a unit rejected by systemd-analyze was reported as clean"
grep -q 'rejected' <<<"$out" || fail "the verify failure is not named: $out"

# 10. The same rejecting analyzer must NOT fail the run when no manager exists:
#     `systemd-analyze --user verify` cannot initialise without the bus and exits
#     non-zero with "Failed to initialize manager", which is not a verdict.
out="$(SYSTEMCTL="$(make_systemctl unreachable)" SYSTEMD_ANALYZE="$analyze_rejects" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" ||
    fail "an unreachable manager made a reachable-verdict check fail: $out"

# 11. Hermeticity guard for this suite itself: a stubbed run must reach the same
#     verdict with the bus env stripped as with it present. This is the case that
#     catches a stubbed SYSTEMCTL paired with the real systemd-analyze, which
#     passes only on a host that happens to have a user manager.
out="$(env -u DBUS_SESSION_BUS_ADDRESS -u XDG_RUNTIME_DIR \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" ||
    fail "a stubbed run without a bus disagreed with a stubbed run with one: $out"

# 12. No HOME and no XDG_CONFIG_HOME either: `set -u` must not turn an unset
#     HOME into a hard failure. With no second argument there is no user unit
#     directory to compare, which is the same exit-0 "nothing to compare" path
#     as case 6.
out="$(env -u HOME -u XDG_CONFIG_HOME -u XDG_RUNTIME_DIR -u DBUS_SESSION_BUS_ADDRESS \
    "$SCRIPT_UNDER_TEST" "$repo" 2>&1)" ||
    fail "an unset HOME was treated as drift: $out"
grep -q 'no user unit directory to compare' <<<"$out" ||
    fail "the unset-HOME note is missing: $out"

echo "check-unit-deployment regression tests: ok"
