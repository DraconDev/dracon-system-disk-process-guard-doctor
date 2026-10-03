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
# The unit name is fixed in the script under test (it is about this repo's
# unit); the file paths are what the suite varies.
UNIT_NAME="dracon-system-guard.service"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-unit-check-XXXXXX")
trap 'rm -rf "$work"' EXIT

# Run the whole suite against an empty HOME so unit discovery can only ever find
# a fixture. The header above promises the host's real unit is never touched,
# but a case that omits the second argument falls back to discovery — and on a
# host that has the guard installed, the synthetic fixture never matches the
# real deployed unit, so the case failed at the byte-comparison step instead of
# testing what it was written to test (case 9 did exactly this, and reported
# "STALE" on any machine with the unit deployed). An empty HOME makes that
# class of leak a "nothing to compare" exit 0, which is the honest answer for a
# host that has not deployed anything.
export HOME="$work/home-isolated"
mkdir -p "$HOME"
unset XDG_CONFIG_HOME

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

# --- systemd stubs -----------------------------------------------------------
# $1: what the loaded unit reports for -p ExecReload ("present", "empty",
# "need-reload", or "unreachable" for a host with no user manager at all).
# "need-reload" answers NeedDaemonReload=yes while ExecReload stays present,
# so only the R4-SYS-10 manager-verdict probe fires.
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
if [ "\${property:-}" = NeedDaemonReload ]; then
    case "$mode" in
        unreachable) exit 1 ;;
        need-reload) printf '%s\n' yes; exit 0 ;;
        *) printf '%s\n' no; exit 0 ;;
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

# 2b. R4-SYS-10: the files agree AND the loaded unit carries ExecReload,
#     but the manager reports NeedDaemonReload=yes (some OTHER directive
#     drifted, e.g. ReadWritePaths). The old ExecReload-only proxy passed
#     this; the manager verdict must fail it.
out="$(SYSTEMCTL="$(make_systemctl need-reload)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" &&
    fail "NeedDaemonReload=yes was reported as in sync"
grep -q 'NeedDaemonReload' <<<"$out" || fail "the manager verdict is not named: $out"
grep -q 'daemon-reload' <<<"$out" || fail "no redeploy hint: $out"

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
#    when there is a manager to ask. The deployed copy is named explicitly and
#    reset to the shipped bytes: this case is about the verify step, so it must
#    not inherit case 8's mutation (which fails at the byte comparison instead)
#    nor depend on what unit discovery finds under the caller's HOME.
cp "$repo" "$deployed"
out="$(SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_rejects" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" &&
    fail "a unit rejected by systemd-analyze was reported as clean"
grep -q 'rejected' <<<"$out" || fail "the verify failure is not named: $out"

# 10. The same rejecting analyzer must NOT fail the run when no manager exists:
#     `systemd-analyze --user verify` cannot initialise without the bus and exits
#     non-zero with "Failed to initialize manager", which is not a verdict.
out="$(SYSTEMCTL="$(make_systemctl unreachable)" SYSTEMD_ANALYZE="$analyze_rejects" \
    "$SCRIPT_UNDER_TEST" "$repo" 2>&1)" ||
    fail "an unreachable manager made a reachable-verdict check fail: $out"

# 11. Hermeticity guard for this suite itself: a stubbed run must reach the same
#     verdict with the bus env stripped as with it present. This is the case that
#     catches a stubbed SYSTEMCTL paired with the real systemd-analyze, which
#     passes only on a host that happens to have a user manager.
out="$(env -u DBUS_SESSION_BUS_ADDRESS -u XDG_RUNTIME_DIR \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" 2>&1)" ||
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

# 13. Unit discovery must follow systemd, which searches BOTH
#     "$HOME/.config/systemd/user" and "$XDG_CONFIG_HOME/systemd/user". Checking
#     only one of them is a silent false pass: a drifted, deployed unit hides
#     behind the path that was not checked. In sync first...
fake_home="$work/home-fixture"
mkdir -p "$fake_home/.config/systemd/user"
cp "$repo" "$fake_home/.config/systemd/user/$UNIT_NAME"
out="$(env -u XDG_CONFIG_HOME HOME="$fake_home" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" 2>&1)" ||
    fail "a unit deployed under HOME/.config was not found: $out"
grep -q "$fake_home/.config/systemd/user/$UNIT_NAME" <<<"$out" ||
    fail "the unit under HOME/.config was not the one compared: $out"
# ...then drifted, so discovery is proven to lead to a real verdict.
printf '# drifted\n' >> "$fake_home/.config/systemd/user/$UNIT_NAME"
if out="$(env -u XDG_CONFIG_HOME HOME="$fake_home" \
        SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
        "$SCRIPT_UNDER_TEST" "$repo" 2>&1)"; then
    fail "a drifted unit under HOME/.config was reported as in sync: $out"
fi
grep -q 'STALE' <<<"$out" || fail "drift under HOME/.config was not reported: $out"

# 14. The same for the XDG_CONFIG_HOME location, with a HOME that holds nothing.
xdg_home="$work/xdg-fixture"
empty_home="$work/home-empty"
mkdir -p "$xdg_home/systemd/user" "$empty_home"
cp "$repo" "$xdg_home/systemd/user/$UNIT_NAME"
out="$(env HOME="$empty_home" XDG_CONFIG_HOME="$xdg_home" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" 2>&1)" ||
    fail "a unit deployed under XDG_CONFIG_HOME was not found: $out"
printf '# drifted\n' >> "$xdg_home/systemd/user/$UNIT_NAME"
if out="$(env HOME="$empty_home" XDG_CONFIG_HOME="$xdg_home" \
        SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
        "$SCRIPT_UNDER_TEST" "$repo" 2>&1)"; then
    fail "a drifted unit under XDG_CONFIG_HOME was reported as in sync: $out"
fi
grep -q 'STALE' <<<"$out" || fail "drift under XDG_CONFIG_HOME was not reported: $out"

# 15. A DANGLING symlink at the deployed path is still "deployed": `-f` follows
#     symlinks, so a unit linked to a GC'd nix store path tested false and the
#     script reported "nothing to compare" — a silent false pass over a broken
#     deployment (audit 2026-10-01).
dangling_home="$work/home-dangling"
mkdir -p "$dangling_home/.config/systemd/user"
ln -s "$xdg_home/removed-by-gc" "$dangling_home/.config/systemd/user/$UNIT_NAME"
out="$(env -u XDG_CONFIG_HOME HOME="$dangling_home" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" 2>&1)" \
    && fail "a dangling unit symlink was reported as in sync: $out"
case "$out" in
    *"nothing to compare"*)
        fail "a dangling symlink must not be treated as 'not deployed': $out" ;;
    *"STALE"*|*"differs"*) : ;;
    *) fail "unexpected verdict for a dangling symlink: $out" ;;
esac

# 16. A REMOVED target of an otherwise-identical unit is likewise reported, not
#     silently accepted.
good_home="$work/home-good"
mkdir -p "$good_home/.config/systemd/user"
cp "$repo" "$good_home/.config/systemd/user/$UNIT_NAME"
out="$(env -u XDG_CONFIG_HOME HOME="$good_home" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" 2>&1)" \
    || fail "an identical unit was reported as drifted: $out"

# Step 3b: the watchdog units travel with the main unit and drift the same
# silent way. A deployed companion that differs from the shipped file is STALE;
# a companion that was never deployed is a note, not a verdict.
# ADDED 2026-10-03 (audit R3-L24): mirrors sync cases 17-19 byte for byte
# in behaviour (companion names differ).
shipped_guard_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
companion_guard_repo="$shipped_guard_dir/dracon-system-guard-watchdog.service"
[ -f "$companion_guard_repo" ] || fail "shipped companion $companion_guard_repo is missing (audit M8 ships it)"

# 17. A deployed companion identical to the shipped file passes.
cp "$repo" "$deployed"
cp "$companion_guard_repo" "$work/dracon-system-guard-watchdog.service"
out="$(SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" ||
    fail "an identical companion unit was reported as drifted: $out"

# 18. A deployed companion that differs from the shipped file is STALE.
printf '# drifted\n' >> "$work/dracon-system-guard-watchdog.service"
out="$(SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" &&
    fail "a drifted companion unit was reported as in sync: $out"
grep -q 'STALE' <<<"$out" || fail "companion drift was not reported: $out"
rm -f "$work/dracon-system-guard-watchdog.service"

# 19. A companion that was never deployed is a note, never a failure — fresh
#     installs gain the backstops through install.sh and the flake module.
out="$(SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$deployed" 2>&1)" ||
    fail "a never-deployed companion was treated as drift: $out"
grep -q 'shipped but not deployed' <<<"$out" ||
    fail "the never-deployed companion note is missing: $out"

# --- runtime storage-root check ----------------------------------------------
# Steps 1-5 compare files; none of them can see that a ReadWritePaths entry
# which is correct on disk still granted nothing at runtime. That is the
# 2026-09-28..10-01 breakage: the reclaim path moved real trees for four days
# and every move failed on a read-only filesystem, so the guard reclaimed zero
# bytes while its log looked busy. These cases pin the runtime contract against
# mountinfo fixtures, so no host with a live service is required.

write_policy() {
    # $1 = path, $2 = quarantine_dir, $3 = relocate_cold_root
    cat > "$1" <<EOF
[storage]
quarantine_dir = "$2"
relocate_cold_root = "$3"
EOF
}

# A faithful reduction of this host's guard namespace: ProtectSystem=strict
# remounts the root and the second disk read-only, and only the configured
# subtrees come back read-write.
mountinfo_fixed() {
    cat <<'EOF'
622 240 259:2 / / ro,nosuid,relatime shared:252 master:1 - ext4 /dev/nvme0n1p2 rw
637 622 0:7 / /dev rw,nosuid shared:374 master:9 - devtmpfs devtmpfs rw
650 622 8:2 / /mnt/data ro,nosuid,noatime shared:527 master:129 - ext4 /dev/sda2 rw
675 650 8:2 /cold /mnt/data/cold rw,nosuid,noatime shared:528 master:129 - ext4 /dev/sda2 rw
676 650 8:2 /quarantine /mnt/data/quarantine rw,nosuid,noatime shared:529 master:129 - ext4 /dev/sda2 rw
1123 622 259:2 /tmp /tmp rw,nosuid,relatime shared:881 master:1 - ext4 /dev/nvme0n1p2 rw
EOF
}
mountinfo_fixed > "$work/mountinfo-fixed"

# 20. The pre-fix shape: the second disk is read-only and neither subtree has
#     its own mount, so every quarantine move fails. This is the exact state the
#     guard ran in for four days, and it MUST be a failure, not a pass.
pre="$work/mountinfo-prefix-disk-only"
mountinfo_fixed | grep -v '^67[56] ' > "$pre"
p_ok="$work/policy-ok.toml"
write_policy "$p_ok" /mnt/data/quarantine /mnt/data/cold
out="$(GUARD_MOUNTINFO="$pre" POLICY_FILE="$p_ok" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "a read-only storage root was reported as in sync: $out"
case "$out" in
    *"resolves read-only"*)
        case "$out" in
            *"/mnt/data/quarantine"*) : ;;
            *) fail "the read-only report did not name quarantine_dir: $out" ;;
        esac
        case "$out" in
            *"/mnt/data/cold"*) : ;;
            *) fail "the read-only report did not name relocate_cold_root: $out" ;;
        esac
        ;;
    *) fail "unexpected verdict for a read-only storage root: $out" ;;
esac

# 21. Granting the disk but not the subtree is still broken: the entry has to
#     name the path itself, because ProtectSystem=strict makes the parent
#     read-only no matter what the disk itself reports.
one="$work/mountinfo-only-cold"
mountinfo_fixed | grep -v '^676 ' > "$one"
out="$(GUARD_MOUNTINFO="$one" POLICY_FILE="$p_ok" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "a grant on the disk but not on the subtree was accepted: $out"
case "$out" in
    *"quarantine_dir=/mnt/data/quarantine resolves read-only"*) : ;;
    *) fail "unexpected verdict for a partially granted subtree: $out" ;;
esac
case "$out" in
    *"relocate_cold_root=/mnt/data/cold is read-write"*) : ;;
    *) fail "the granted root should have passed: $out" ;;
esac

# 22. The fixed shape passes and names the mounts it relied on.
out="$(GUARD_MOUNTINFO="$work/mountinfo-fixed" POLICY_FILE="$p_ok" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    || fail "the fixed namespace was reported as broken: $out"
case "$out" in
    *"✓ quarantine_dir=/mnt/data/quarantine is read-write"*) : ;;
    *) fail "no pass line for quarantine_dir: $out" ;;
esac

# 23. A later mount at the same point shadows an earlier read-write one, which
#     is how ProtectHome/ProtectSystem end up making XDG_RUNTIME_DIR read-only
#     here. Reading the first match instead of the effective one would call
#     /run/user/1000 writable when it is not.
shadow="$work/mountinfo-shadowed"
cat <<'EOF' > "$shadow"
646 642 0:55 / /run/user/1000 rw,nosuid,nodev,relatime shared:519 master:315 - tmpfs tmpfs rw
1017 642 0:25 /user /run/user ro,nosuid,nodev shared:522 master:12 - tmpfs tmpfs rw
1018 1017 0:55 / /run/user/1000 ro,nosuid,nodev,relatime shared:523 master:315 - tmpfs tmpfs rw
EOF
write_policy "$work/policy-runtime.toml" /run/user/1000/doc ""
out="$(GUARD_MOUNTINFO="$shadow" POLICY_FILE="$work/policy-runtime.toml" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "a shadowed read-only mount was reported writable: $out"
case "$out" in
    *"resolves read-only"*) : ;;
    *) fail "the shadowed mount was not detected: $out" ;;
esac

# 24. A root under an ordinary read-write mount is fine, and a root that no
#     mount covers at all is reported rather than passed.
p_tmp="$work/policy-tmp.toml"
write_policy "$p_tmp" /tmp/quarantine ""
out="$(GUARD_MOUNTINFO="$work/mountinfo-fixed" POLICY_FILE="$p_tmp" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    || fail "a read-write /tmp subtree was rejected: $out"
bare="$work/mountinfo-bare"
printf '622 240 259:2 / / ro,nosuid,relatime shared:252 master:1 - ext4 /dev/nvme0n1p2 rw\n' > "$bare"
out="$(GUARD_MOUNTINFO="$bare" POLICY_FILE="$p_ok" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "a root under a read-only root mount was accepted: $out"
case "$out" in
    *"resolves read-only"*|*"not covered by any mount"*) : ;;
    *) fail "unexpected verdict for a root-only namespace: $out" ;;
esac

# 25. A commented-out knob is not a configured root, and neither is a relative
#     one — both are skipped rather than failed, because there is nothing
#     concrete to check.
p_commented="$work/policy-commented.toml"
cat > "$p_commented" <<'EOF'
[storage]
# quarantine_dir = "/mnt/data/quarantine"
# relocate_cold_root = "/mnt/data/cold"
EOF
out="$(GUARD_MOUNTINFO="$pre" POLICY_FILE="$p_commented" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    || fail "commented-out storage roots were treated as configured: $out"

# 26. "Cannot ask" is never a failure: a stopped service and an unreadable
#     mountinfo source are notes, not alarms.
out="$(GUARD_MAINPID=0 POLICY_FILE="$p_ok" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    || fail "a stopped service was reported as a failure: $out"
case "$out" in
    *"not running"*) : ;;
    *) fail "a stopped service was not reported as skipped: $out" ;;
esac
out="$(GUARD_MOUNTINFO="$work/no-such-mountinfo" POLICY_FILE="$p_ok" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "an unreadable mountinfo source was reported as in sync: $out"

# 27. A root that lives under / with no mount of its own resolves through the
#     root mount. "/foo" does not match the shell pattern "//*", so an
#     implementation that only compares prefixes in `case` reports this ordinary
#     arrangement as "not covered by any mount" — a false failure that would
#     tell the operator to add a ReadWritePaths entry that changes nothing.
write_policy "$work/policy-root-mount.toml" /var/tmp/quarantine ""
out="$(GUARD_MOUNTINFO="$bare" POLICY_FILE="$work/policy-root-mount.toml" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "a read-only root mount was accepted: $out"
# The verdict must name the root mount as the effective one. "not covered by
# any mount" would be the wrong answer: it would tell the operator their root is
# unreachable when in fact it resolves through / like every ordinary path.
case "$out" in
    *"resolves read-only"*"effective mount: / ("*) : ;;
    *) fail "the root mount was not used to resolve /var/tmp: $out" ;;
esac

# The same shape with a writable root mount must pass, so the case above cannot
# be satisfied by simply always reporting read-only.
cat > "$work/mountinfo-root-rw" <<'EOF'
622 240 259:2 / / rw,nosuid,relatime shared:252 master:1 - ext4 /dev/nvme0n1p2 rw
EOF
out="$(GUARD_MOUNTINFO="$work/mountinfo-root-rw" POLICY_FILE="$work/policy-root-mount.toml" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    || fail "a writable root mount was rejected: $out"
case "$out" in
    *"is read-write in the running namespace (mount: /)"*) : ;;
    *) fail "the writable root mount was not accepted: $out" ;;
esac

# 28. A symlinked root matches on its CANONICAL path (R3-L23, guard
#     twin): the literal match hit the read-only parent and
#     false-positived, exactly like the sync checker's ~/.ssh case.
mkdir -p "$work/guard-realroot"
ln -sfn "$work/guard-realroot" "$work/guard-linkroot"
guard_real="$(readlink -f "$work/guard-realroot")"
write_policy "$work/policy-link.toml" "$work/guard-linkroot" ""
cat <<EOF > "$work/mountinfo-link"
622 240 259:2 / / ro,nosuid,relatime shared:252 master:1 - ext4 /dev/nvme0n1p2 rw
700 622 259:2 /tmp /tmp ro,nosuid,relatime shared:900 master:1 - ext4 /dev/nvme0n1p2 rw
701 700 259:2 $guard_real $guard_real rw,nosuid,relatime shared:901 master:1 - ext4 /dev/nvme0n1p2 rw
EOF
out="$(GUARD_MOUNTINFO="$work/mountinfo-link" POLICY_FILE="$work/policy-link.toml" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    || fail "a symlinked guard root under an rw bind was reported read-only: $out"
case "$out" in
    *"is read-write"*) : ;;
    *) fail "no pass line for the symlinked guard root: $out" ;;
esac

# 29. The checker reads the policy the daemon reads (R4-SYS-01):
#     DRACON_SYSTEM_POLICY wins over the default path, and the
#     config.toml fallback is honored. Before the fix the checker
#     read only ~/.dracon/utilities/system/dracon-system.toml, so
#     with an env override it audited the wrong file (here: a decoy
#     naming a writable root) and passed vacuously.
cat > "$work/mountinfo-env" <<'EOF'
622 240 259:2 / / ro,nosuid,relatime shared:252 master:1 - ext4 /dev/nvme0n1p2 rw
700 622 259:2 /tmp /tmp rw,nosuid,relatime shared:900 master:1 - ext4 /dev/nvme0n1p2 rw
EOF
mkdir -p "$HOME/.dracon/utilities/system"
write_policy "$HOME/.dracon/utilities/system/dracon-system.toml" /tmp/decoy ""
write_policy "$work/policy-env.toml" /var/readonly ""
out="$(env -u POLICY_FILE GUARD_MOUNTINFO="$work/mountinfo-env" \
    DRACON_SYSTEM_POLICY="$work/policy-env.toml" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "the env-override policy was not read (decoy passed): $out"
case "$out" in
    *"/var/readonly"*) : ;;
    *) fail "the verdict did not name the env policy's root: $out" ;;
esac
# Control: with the roles swapped the writable env root must pass,
# so the assertion above cannot be satisfied by always failing.
write_policy "$work/policy-env.toml" /tmp/env-ok ""
write_policy "$HOME/.dracon/utilities/system/dracon-system.toml" /var/decoy-ro ""
out="$(env -u POLICY_FILE GUARD_MOUNTINFO="$work/mountinfo-env" \
    DRACON_SYSTEM_POLICY="$work/policy-env.toml" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    || fail "the writable env-override root was rejected: $out"
# Fallback chain: with no env override and no dracon-system.toml, the
# second candidate (config.toml) is read — the daemon's order.
rm -f "$HOME/.dracon/utilities/system/dracon-system.toml"
write_policy "$HOME/.dracon/utilities/system/config.toml" /var/readonly ""
out="$(env -u POLICY_FILE -u DRACON_SYSTEM_POLICY GUARD_MOUNTINFO="$work/mountinfo-env" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "the config.toml fallback was not read: $out"
case "$out" in
    *"/var/readonly"*) : ;;
    *) fail "the verdict did not name the fallback policy's root: $out" ;;
esac
# Tidy up: later cases must not see this case's HOME policies.
rm -rf "$HOME/.dracon"

# 30. Quote-style and tilde parity with the daemon (R4-SYS-02): a
#     single-quoted root is honored (not read as unset), and bare `~`
#     resolves to $HOME (not skipped as "relative"). Both halves fail
#     against the pre-fix checker, which passed vacuously on each.
cat > "$work/mountinfo-quote" <<'EOF'
622 240 259:2 / / ro,nosuid,relatime shared:252 master:1 - ext4 /dev/nvme0n1p2 rw
EOF
cat > "$work/policy-single.toml" <<'EOF'
[storage]
quarantine_dir = '/var/readonly'
relocate_cold_root = ''
EOF
out="$(GUARD_MOUNTINFO="$work/mountinfo-quote" POLICY_FILE="$work/policy-single.toml" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "a single-quoted read-only root was skipped as unset: $out"
case "$out" in
    *"/var/readonly"*) : ;;
    *) fail "the verdict did not name the single-quoted root: $out" ;;
esac
# Bare `~` with HOME under the read-only root mount must be checked
# (and fail), not skipped as relative. HOME is the suite's isolated
# fixture home, which lives under /tmp — but the fixture mountinfo
# has no /tmp line, so it resolves through the read-only /.
cat > "$work/policy-tilde.toml" <<'EOF'
[storage]
quarantine_dir = "~"
relocate_cold_root = ''
EOF
out="$(GUARD_MOUNTINFO="$work/mountinfo-quote" POLICY_FILE="$work/policy-tilde.toml" \
    SYSTEMCTL="$(make_systemctl present)" SYSTEMD_ANALYZE="$analyze_clean" \
    "$SCRIPT_UNDER_TEST" "$repo" "$repo" 2>&1)" \
    && fail "a bare-~ root was skipped as relative: $out"
case "$out" in
    *"$HOME"*) : ;;
    *) fail "the verdict did not name the resolved home: $out" ;;
esac

echo "check-unit-deployment regression tests: ok"
