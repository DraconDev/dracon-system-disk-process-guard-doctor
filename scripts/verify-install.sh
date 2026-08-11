#!/usr/bin/env bash
# scripts/verify-install.sh — post-install / pre-release fixture check for a
# dracon-system binary.
#
# This validates the packaged artifact's installed CLI surface without using
# the source checkout: the binary must report a semantic version and produce
# valid JSON from the read-only status command with the expected schema.
#
# Usage: scripts/verify-install.sh [binary-path]
# Exit codes: 0 = fixture clean; 1 = fixture failed.
set -euo pipefail

BIN="${1:-dracon-system}"
if ! command -v "$BIN" >/dev/null 2>&1; then
    echo "✗ binary '$BIN' not found on PATH" >&2
    exit 1
fi

VERSION_OUT="$("$BIN" --version 2>&1)" || {
    echo "✗ FAIL: '$BIN --version' failed" >&2
    exit 1
}
if ! grep -Eq '^dracon-system [0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.-]+)?$' <<<"$VERSION_OUT"; then
    echo "✗ FAIL: unexpected version output from '$BIN': $VERSION_OUT" >&2
    exit 1
fi

STATUS_JSON="$("$BIN" status --json 2>/dev/null)" || {
    echo "✗ FAIL: '$BIN status --json' failed" >&2
    exit 1
}
if ! printf '%s\n' "$STATUS_JSON" | python3 -c '
import json
import sys

report = json.load(sys.stdin)
required = {
    "system_root": str,
    "nixos_root": str,
    "sync_policy": str,
    "system_policy": str,
    "system_policy_exists": bool,
    "sync_service_active": bool,
}
if any(not isinstance(report.get(key), kind) for key, kind in required.items()):
    raise SystemExit(1)
'; then
    echo "✗ FAIL: '$BIN status --json' returned an unexpected JSON schema" >&2
    exit 1
fi

echo "✓ OK: '$BIN' reports a valid version and status JSON schema."
