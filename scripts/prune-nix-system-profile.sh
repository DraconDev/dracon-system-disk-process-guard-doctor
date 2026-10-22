#!/usr/bin/env bash
# Prune the ROOT-OWNED Nix system profile generations.
#
# WHY THIS EXISTS (2026-10-22): the guard runs in the user session and can
# never prune /nix/var/nix/profiles/default — nix-env answers "Read-only
# file system (needs root; user GC still runs)". The guard only attempts
# the prune when the generation count is above the retention, so that
# expected permission failure is no longer a line on every cycle; when the
# prune IS needed, this script is the apply-ready privileged path.
#
# Usage (needs root):
#   sudo dracon-system/scripts/prune-nix-system-profile.sh [keep_generations]
#
# The default retention matches the guard's nix_keep_generations default
# (5). The guard's own user-profile prune needs no privilege and keeps
# running either way; only the system profile is root-owned.
set -euo pipefail

PROFILE="${NIX_SYSTEM_PROFILE:-/nix/var/nix/profiles/default}"
KEEP="${1:-${NIX_KEEP_GENERATIONS:-5}}"

if ! [ "$KEEP" -ge 1 ]; then
  echo "keep_generations must be at least 1" >&2
  exit 2
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "this prunes a root-owned profile — run it with sudo" >&2
  exit 2
fi

if ! command -v nix-env >/dev/null 2>&1; then
  echo "nix-env is not on PATH" >&2
  exit 2
fi

if [ ! -e "$PROFILE" ]; then
  echo "no such profile: $PROFILE" >&2
  exit 2
fi

echo "generations before: $(nix-env --list-generations -p "$PROFILE" | wc -l)"
# "+N" keeps the newest N generations; a bare number would delete exactly
# that one generation (the same reading the guard uses).
nix-env --delete-generations "+$KEEP" -p "$PROFILE"
echo "generations after:  $(nix-env --list-generations -p "$PROFILE" | wc -l)"
echo "done — the ordinary user GC reclaims the store paths they held"
