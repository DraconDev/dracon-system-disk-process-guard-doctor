#!/usr/bin/env bash
# Bounded, read-only retry gate for the whole-system space measurement.
#
# Each attempt warms Nix reachability first, waits for daemon-released query
# roots to clear, then captures and preserves the exact pre-sweep root list.
# That list is passed directly into the anchored pass as roots-before; no
# warm-up or second pre-sweep sample can move the baseline. The pass qualifies
# only when the preserved baseline, roots-before, and roots-after counts agree
# and systemd/journal evidence shows no nix-gc.service run in the bounded
# window. Five-count stability samples from earlier attempts remain retained
# as diagnostics, but the acceptance check is the actual start/end bracket.
set -uo pipefail

SCRIPT=${ANCHORED_PASS_SCRIPT:-/home/dracon/.cache/space-hygiene/anchored-pass.sh}
LOGDIR=${SPACE_HYGIENE_LOGDIR:-/home/dracon/.cache/space-hygiene/attempts}
mkdir -p "$LOGDIR"
MAX_ATTEMPTS=${MAX_ATTEMPTS:-3}

for attempt in $(seq 1 "$MAX_ATTEMPTS"); do
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  pre="$LOGDIR/pre-$stamp.txt"
  baseline="$LOGDIR/baseline-$stamp.txt"
  run="$LOGDIR/run-$stamp.txt"
  stdout="$LOGDIR/run-$stamp.stdout"
  before="$LOGDIR/before-$stamp.txt"
  after="$LOGDIR/after-$stamp.txt"
  gc_window="$LOGDIR/gc-window-$stamp.txt"
  prewarm_roots="$LOGDIR/warmup-roots-$stamp.txt"
  prewarm_targets="$LOGDIR/warmup-targets-$stamp.txt"
  prewarm_reach="$LOGDIR/warmup-reach-$stamp.txt"
  window_start=$(date -u +%FT%TZ)
  gc_start_before_rc=0
  gc_start_before=$(systemctl show nix-gc.service -p ExecMainStartTimestamp --value 2>"$LOGDIR/gc-start-before-$stamp.err") || gc_start_before_rc=$?
  printf '=== attempt %s @ %s ===\n' "$attempt" "$stamp" | tee -a "$LOGDIR/attempts.log"
  printf 'observation_window_start=%s gc_last_start_before=%s gc_start_before_status=%s\n' \
    "$window_start" "$gc_start_before" "$gc_start_before_rc" | tee -a "$pre" "$LOGDIR/attempts.log"
  if [ "$gc_start_before_rc" -ne 0 ] || [ -z "$gc_start_before" ] || [ "$gc_start_before" = n/a ]; then
    echo "attempt $attempt: cannot establish nix-gc.service start baseline; refusing to qualify" | tee -a "$LOGDIR/attempts.log"
    exit 2
  fi

  # Warm up reachability queries before selecting the pre-sweep baseline.
  if ! nix-store --gc --print-roots >"$prewarm_roots" 2>"$LOGDIR/warmup-roots-$stamp.err"; then
    echo "attempt $attempt: root listing failed during warm-up; skipping" | tee -a "$LOGDIR/attempts.log"
    continue
  fi
  awk '{if ($NF ~ /^\/nix\/store\//) print $NF}' "$prewarm_roots" >"$prewarm_targets"
  if [ ! -s "$prewarm_targets" ] || ! xargs -r -d '\n' -n 40 nix-store -qR <"$prewarm_targets" 2>"$LOGDIR/warmup-reach-$stamp.err" | sort -u >"$prewarm_reach"; then
    echo "attempt $attempt: reachability warm-up failed; skipping" | tee -a "$LOGDIR/attempts.log"
    continue
  fi
  warmup_reachable=$(wc -l <"$prewarm_reach")
  echo "warmup_reachable=$warmup_reachable" | tee -a "$pre"
  sleep 10

  # This saved list is the true pre-sweep baseline. The anchored pass consumes
  # this exact file as roots-before and begins candidate measurements directly.
  baseline_raw="$LOGDIR/baseline-raw-$stamp.txt"
  if ! nix-store --gc --print-roots >"$baseline_raw" 2>"$LOGDIR/baseline-$stamp.err" || [ ! -s "$baseline_raw" ]; then
    echo "attempt $attempt: pre-sweep root listing failed; skipping" | tee -a "$LOGDIR/attempts.log"
    continue
  fi
  sort "$baseline_raw" >"$baseline"
  baseline_count=$(wc -l <"$baseline")
  baseline_at=$(date -u +%FT%TZ)
  if [ "$baseline_count" -le 0 ]; then
    echo "attempt $attempt: empty pre-sweep baseline; refusing to qualify" | tee -a "$LOGDIR/attempts.log"
    continue
  fi
  printf 'pre_sweep_baseline_at=%s pre_sweep_baseline_count=%s baseline_file=%s\n' \
    "$baseline_at" "$baseline_count" "$baseline" | tee -a "$pre" "$LOGDIR/attempts.log"
  echo "attempt $attempt: starting anchored pass from preserved baseline" | tee -a "$LOGDIR/attempts.log"

  OUT="$run" \
  ROOTS_BEFORE_FILE="$before" \
  ROOTS_AFTER_FILE="$after" \
  ROOTS_BEFORE_INPUT="$baseline" \
  BASELINE_FILE="$baseline" \
  BASELINE_AT="$baseline_at" \
  PREWARM_DONE=YES \
  WARMUP_REACHABLE="$warmup_reachable" \
  ROOT_TARGETS_FILE="$LOGDIR/root-targets-$stamp.txt" \
  REACH_FILE="$LOGDIR/reach-$stamp.txt" \
  SERIES_FILE="$LOGDIR/series-$stamp.txt" \
  MEAS_FILE="$LOGDIR/meas-$stamp.txt" \
  STOPFILE="$LOGDIR/sampler-stop-$stamp" \
    bash "$SCRIPT" >"$stdout" 2>&1
  rc=$?
  echo "attempt $attempt: pass exit=$rc" | tee -a "$LOGDIR/attempts.log"
  grep -E '^(pre_sweep_baseline_at|pre_sweep_baseline_count|pre_sweep_baseline_file|roots_before|roots_after|roots_unchanged|root_set_identical|root_series_samples|root_series_min|root_series_max|root_series_constant|root_series_transitions|store_paths_before|store_paths_after|pass_seconds)=' "$run" | tee -a "$LOGDIR/attempts.log"

  gc_start_after_rc=0
  gc_start_after=$(systemctl show nix-gc.service -p ExecMainStartTimestamp --value 2>"$LOGDIR/gc-start-after-$stamp.err") || gc_start_after_rc=$?
  journal_rc=0
  journalctl -u nix-gc.service --since "$window_start" --no-pager >"$gc_window" 2>&1 || journal_rc=$?
  if [ "$gc_start_after_rc" -ne 0 ] || [ -z "$gc_start_after" ] || [ "$gc_start_before" != "$gc_start_after" ] || [ "$journal_rc" -ne 0 ] || \
     grep -qiE 'permission denied|access denied|not authorized|failed to open|no journal files were found' "$gc_window"; then
    gc_unchanged=UNKNOWN
  elif grep -qiE 'nix-gc\.service:|nix-gc-start|Nix Garbage Collector|deleting garbage|store paths deleted' "$gc_window"; then
    gc_unchanged=NO
  else
    gc_unchanged=YES
  fi

  roots_before=$(sed -n 's/^roots_before=//p' "$run" | head -n 1)
  roots_after=$(sed -n 's/^roots_after=//p' "$run" | head -n 1)
  reported_baseline=$(sed -n 's/^pre_sweep_baseline_count=//p' "$run" | head -n 1)
  roots_unchanged=$(sed -n 's/^roots_unchanged=//p' "$run" | head -n 1)
  baseline_matches_before=NO
  if [ -s "$before" ] && cmp -s "$baseline" "$before"; then
    baseline_matches_before=YES
  fi
  printf 'preserved_baseline_count=%s run_baseline_count=%s roots_before=%s roots_after=%s baseline_matches_before=%s gc_unchanged=%s\n' \
    "$baseline_count" "$reported_baseline" "$roots_before" "$roots_after" "$baseline_matches_before" "$gc_unchanged" | tee -a "$LOGDIR/attempts.log"

  if [ "$rc" -eq 0 ] && [ "$reported_baseline" = "$baseline_count" ] && \
     [ "$roots_before" = "$baseline_count" ] && [ "$roots_after" = "$baseline_count" ] && \
     [ "$roots_unchanged" = YES ] && [ "$baseline_matches_before" = YES ] && [ "$gc_unchanged" = YES ]; then
    echo "attempt $attempt: SUCCESS - measured roots match preserved pre-sweep baseline; no nix-gc.service run in window" | tee -a "$LOGDIR/attempts.log"
    exit 0
  fi
  if [ "$gc_unchanged" != YES ]; then
    echo "attempt $attempt: no-GC condition unavailable or violated; aborting retries" | tee -a "$LOGDIR/attempts.log"
    exit 2
  fi
  echo "attempt $attempt: baseline or post-pass count differed; retrying with a fresh preserved baseline" | tee -a "$LOGDIR/attempts.log"
done

echo "all $MAX_ATTEMPTS attempts exhausted without a qualifying bounded pass" | tee -a "$LOGDIR/attempts.log"
exit 1
