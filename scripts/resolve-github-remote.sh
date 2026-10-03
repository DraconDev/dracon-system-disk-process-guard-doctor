#!/usr/bin/env bash
# scripts/resolve-github-remote.sh — print the name of the git remote whose
# URL points at github.com.
#
# Ported from dracon-sync v0.113.11 (2026-10-03, audit R4-M-10):
# dracon-system's release.sh hardcoded REMOTE=origin and failed on any
# non-origin remote naming — the identical class sync fixed by deriving
# the remote from remote.*.url. Standalone so the derivation is directly
# testable against fixture repos.
#
# Usage:
#   scripts/resolve-github-remote.sh [repo-path]
#
# Resolution order:
#   1. Remote whose URL names the canonical dracon-system repo.
#   2. Otherwise the first github.com remote (sorted); a warning goes to
#      stderr when there are several.
#
# Exit codes:
#   0  remote name printed on stdout
#   2  no github.com remote configured (loud message on stderr)
set -euo pipefail

REPO="${1:-$(git -C "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" rev-parse --show-toplevel)}"
CANON="dracon-system-disk-process-guard-doctor"

names=()
while IFS= read -r line; do
    key="${line%% *}"
    url="${line#* }"
    case "${url,,}" in
        *github.com*) ;;
        *) continue ;;
    esac
    name="${key#remote.}"
    name="${name%.url}"
    names+=("$name")
done < <(git -C "$REPO" config --get-regexp '^remote\..*\.url$' || true)

if [[ ${#names[@]} -eq 0 ]]; then
    printf '✗ no github.com remote configured in %s; add one (git remote add <name> <github-url>) or pass --remote <name>\n' "$REPO" >&2
    exit 2
fi

# Prefer the canonical-repo URL.
for n in "${names[@]}"; do
    u="$(git -C "$REPO" config --get "remote.$n.url")"
    if [[ "$u" == *"$CANON"* ]]; then
        printf '%s\n' "$n"
        exit 0
    fi
done

# Otherwise: deterministic choice (sorted first), loud about ambiguity.
# FIXED 2026-09-27 (audit rework round 3, F86): the previous
# `IFS=$'\n' sorted=($(...))` split command output by unquoted expansion
# (shellcheck SC2207), which also word-splits any remote name containing
# whitespace. `mapfile -t` reads the sorted lines one-per-element with no
# splitting and no subshell.
mapfile -t sorted < <(printf '%s\n' "${names[@]}" | sort)
if [[ ${#sorted[@]} -gt 1 ]]; then
    printf '⚠ multiple github remotes (%s); using %s\n' "${sorted[*]}" "${sorted[0]}" >&2
fi
printf '%s\n' "${sorted[0]}"
