#!/usr/bin/env bash
# Regression tests for verify-install.sh's packaged-binary fixture.
set -euo pipefail

SCRIPT_UNDER_TEST="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/verify-install.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/dracon-system-verify-install-XXXXXX")
trap 'rm -rf "$work"' EXIT

fake="$work/dracon-system"
cat > "$fake" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
    --version)
        printf '%s\n' 'dracon-system 9.9.9'
        ;;
    status)
        printf '%s\n' '{"system_root":"/tmp/.dracon","nixos_root":"/tmp/.dracon/nixos","sync_policy":"/tmp/sync.toml","system_policy":"/tmp/system.toml","system_policy_exists":false,"sync_service_active":false}'
        ;;
    *)
        exit 2
        ;;
esac
EOF
chmod +x "$fake"

"$SCRIPT_UNDER_TEST" "$fake" >/dev/null

bad="$work/bad-dracon-system"
sed 's/dracon-system 9.9.9/not-a-system-binary/' "$fake" > "$bad"
chmod +x "$bad"
if "$SCRIPT_UNDER_TEST" "$bad" >/dev/null 2>&1; then
    echo "invalid version fixture unexpectedly passed" >&2
    exit 1
fi

# 2026-10-01 (audit): the check only validated the SHAPE of --version, so a
# stale binary at the fixture root passed the pre-release gate. An explicit
# expected version must be enforced.
if "$SCRIPT_UNDER_TEST" "$fake" 9.9.9 >/dev/null 2>&1; then
    echo "a stale binary passed when an expected version was given" >&2
    exit 1
fi
"$SCRIPT_UNDER_TEST" "$fake" 9.9.9.1 >/dev/null 2>&1 && {
    echo "a version mismatch was accepted" >&2
    exit 1
}
# The happy path still passes when the expected version matches.
"$SCRIPT_UNDER_TEST" "$fake" 9.9.9 >/dev/null

# 2026-10-01 (audit): a missing python3 was reported as a JSON schema failure,
# which points at the wrong cause entirely. Run with an empty PATH so python3
# cannot be found, and require the interpreter-specific message.
empty_path="$work/empty-bin"
mkdir -p "$empty_path"
# A stub `dracon-system` is still needed; only the interpreter is missing.
ln -sf "$fake" "$empty_path/dracon-system"
out=$(PATH="$empty_path" "$SCRIPT_UNDER_TEST" "$empty_path/dracon-system" 2>&1) && {
    echo "a missing python3 was accepted" >&2
    exit 1
}
case "$out" in
    *python3*) : ;;
    *) echo "missing python3 was misdiagnosed: $out" >&2; exit 1 ;;
esac

echo "verify-install regression tests: ok"
