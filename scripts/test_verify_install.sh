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

echo "verify-install regression tests: ok"
