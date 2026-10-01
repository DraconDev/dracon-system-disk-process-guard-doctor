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
# The fixture reports 9.9.9: asking for that must pass.
"$SCRIPT_UNDER_TEST" "$fake" 9.9.9 >/dev/null || {
    echo "a matching expected version was rejected" >&2
    exit 1
}
# Asking for a different version must fail — that is the stale-binary case.
if "$SCRIPT_UNDER_TEST" "$fake" 9.9.8 >/dev/null 2>&1; then
    echo "a stale binary passed the expected-version check" >&2
    exit 1
fi
if out="$("$SCRIPT_UNDER_TEST" "$fake" 9.9.8 2>&1)"; then
    :
else
    case "$out" in
        *"this release is 9.9.8"*) : ;;
        *) echo "version mismatch was misdiagnosed: $out" >&2; exit 1 ;;
    esac
fi

# 2026-10-01 (audit): a missing python3 was reported as a JSON schema failure,
# which points at the wrong cause entirely. Run with an empty PATH so python3
# cannot be found, and require the interpreter-specific message.
# Build a PATH that has what the script itself needs (its #!/usr/bin/env bash
# interpreter plus the few utilities it calls) but deliberately NO python3.
nopy_path="$work/nopython-bin"
mkdir -p "$nopy_path"
for tool in bash env grep sed cat; do
    resolved="$(command -v "$tool" 2>/dev/null)" && ln -sf "$resolved" "$nopy_path/$tool"
done
ln -sf "$fake" "$nopy_path/dracon-system"
out=$(PATH="$nopy_path" "$SCRIPT_UNDER_TEST" "$nopy_path/dracon-system" 2>&1) && {
    echo "a missing python3 was accepted" >&2
    exit 1
}
case "$out" in
    *python3*) : ;;
    *) echo "missing python3 was misdiagnosed: $out" >&2; exit 1 ;;
esac

echo "verify-install regression tests: ok"
