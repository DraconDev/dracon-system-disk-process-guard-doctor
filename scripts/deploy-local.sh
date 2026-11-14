#!/usr/bin/env bash
# scripts/deploy-local.sh — build, install and restart the guard on THIS host.
#
# Local only: no publish, no tag, no push (that is scripts/release.sh).
#
# WHY (2026-11-14): a hand deploy raced a concurrent `systemctl start` — the
# guard came up on the OLD binary, the new one was installed afterwards, and
# `systemctl start` on an already-active unit is a no-op. The guard ran the
# replaced binary (exe "(deleted)") while every health check was green.
# This script always RESTARTS and then proves the running exe is the
# installed file.
#
# Usage: scripts/deploy-local.sh [--no-build]
# Env:   DRACON_SYSTEM_BIN (default ~/.local/bin/dracon-system)
#        DRACON_SYSTEM_UNIT (default dracon-system-guard.service)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${DRACON_SYSTEM_BIN:-$HOME/.local/bin/dracon-system}"
unit="${DRACON_SYSTEM_UNIT:-dracon-system-guard.service}"
build=1
[[ "${1:-}" == "--no-build" ]] && build=0

cd "$here"
target_dir="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
artifact="${target_dir:-$here/target}/release/dracon-system"

if (( build )); then
    echo "▶ cargo build --release --locked -p dracon-system"
    cargo build --release --locked -p dracon-system
fi
[[ -x "$artifact" ]] || { echo "✗ no release artifact at $artifact" >&2; exit 1; }

echo "▶ artifact: $("$artifact" --version)"
if [[ -e "$bin" ]]; then
    backup="$bin.bak-$(date +%Y%m%d-%H%M%S)"
    cp -p "$bin" "$backup"
    echo "▶ backup:   $backup"
fi

# Atomic replace: install to a sibling temp name, then rename over the
# target. Never write into the running inode (ETXTBSY / torn binary).
tmp="$(dirname "$bin")/.dracon-system.new.$$"
install -m755 "$artifact" "$tmp"
mv -f "$tmp" "$bin"
cmp -s "$artifact" "$bin" || { echo "✗ installed binary differs from artifact" >&2; exit 1; }

echo "▶ systemctl --user restart $unit"
systemctl --user restart "$unit"

# Prove the running process is the file we just installed.
for _ in $(seq 1 20); do
    pid="$(systemctl --user show "$unit" -p MainPID --value)"
    if [[ -n "$pid" && "$pid" != 0 ]]; then
        exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
        # Right after exec the link can still name systemd's executor.
        if [[ "$exe" == "$bin" ]]; then
            echo "✓ $unit pid=$pid runs $exe"
            journalctl --user -u "$unit" --since "-20s" --no-pager -o cat | tail -5 || true
            exit 0
        fi
        if [[ "$exe" == *" (deleted)" ]]; then
            echo "✗ $unit pid=$pid still runs a replaced binary: $exe" >&2
            exit 1
        fi
    fi
    sleep 0.5
done
echo "✗ $unit did not come up on $bin (pid=${pid:-none}, exe=${exe:-unknown})" >&2
systemctl --user status "$unit" --no-pager | head -15 >&2 || true
exit 1
