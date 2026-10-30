#!/usr/bin/env bash
set -euo pipefail
baseline_count=$(wc -l < "$ROOTS_BEFORE_INPUT")
cp "$ROOTS_BEFORE_INPUT" "$ROOTS_BEFORE_FILE"
{
  printf 'pre_sweep_baseline_at=%s\n' "$BASELINE_AT"
  printf 'pre_sweep_baseline_count=%s\n' "$baseline_count"
  printf 'pre_sweep_baseline_file=%s\n' "$BASELINE_FILE"
  printf 'roots_before=%s\n' "$baseline_count"
} > "$OUT"
after_count=${MOCK_ROOTS_AFTER_COUNT:-$baseline_count}
if [ "${MOCK_ROOTS_AFTER_CHANGED:-NO}" = YES ] && [ "$after_count" -eq "$baseline_count" ]; then
  printf '%s\n' '/different/profile -> /nix/store/another-profile' > "$ROOTS_AFTER_FILE"
elif [ "$after_count" -eq "$baseline_count" ]; then
  cp "$ROOTS_BEFORE_INPUT" "$ROOTS_AFTER_FILE"
else
  : > "$ROOTS_AFTER_FILE"
  for ((i=0; i<after_count; i++)); do printf '/root/%d -> /nix/store/mock-%d\n' "$i" "$i" >> "$ROOTS_AFTER_FILE"; done
fi
{
  printf 'roots_after=%s\n' "$after_count"
  if [ "$baseline_count" -eq "$after_count" ]; then printf 'roots_unchanged=YES\n'; else printf 'roots_unchanged=NO\n'; fi
  if cmp -s "$ROOTS_BEFORE_INPUT" "$ROOTS_AFTER_FILE"; then printf 'root_set_identical=YES\n'; else printf 'root_set_identical=NO\n'; fi
  printf 'store_paths_before=100\nstore_paths_after=100\npass_seconds=1\n'
} >> "$OUT"
