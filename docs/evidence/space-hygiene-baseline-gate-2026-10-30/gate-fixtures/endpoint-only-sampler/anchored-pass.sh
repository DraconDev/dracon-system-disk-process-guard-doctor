#!/usr/bin/env bash
# Anchored space-hygiene measurement pass v5 (read-only).
#
# Instrument history and the three defects it fixes:
#
#  1. Warm-up (v1 defect). `nix-store -qR <path>` registers the queried store
#     path as a transient indirect GC root in the daemon while the query runs.
#     v1 bracketed roots_before BEFORE any closure query, so the "after"
#     reading always carried query-induced roots and could never match. v5
#     runs the same closure query as a warm-up, then brackets with the
#     identical query, so both sides are taken in the same daemon state.
#
#  2. Explicit PID waits (v4 defect). v4 used a bare `wait`, which also
#     waited for the sampler subshell and therefore never returned. v5
#     tracks measurement PIDs and waits only on those.
#
#  3. Parallel measurements (v2 defect). v2 took 1564s serially; v5 runs the
#     independent du/find measurements concurrently, so the root-count
#     bracket spans the slowest single measurement instead of their sum.
#
#  4. Continuous series. The endpoint pair alone is misleading: the daemon
#     registers a store path as a temporary GC root while any live process
#     holds a file in it, so entries appear and disappear on a seconds-long
#     cycle. v5 samples the root count every 5s across the whole pass and
#     reports the full series (min/max/distinct/transitions) alongside the
#     endpoints.
#
# No mutating command is executed: only du/find/ls/readlink/journalctl/
# systemctl reads plus nix-store/nix queries documented as read-only.
set -uo pipefail

OUT=${OUT:-/tmp/space-hygiene-anchored-pass.txt}
ROOTS_BEFORE=${ROOTS_BEFORE_FILE:-/tmp/space-hygiene-roots-before.txt}
ROOTS_AFTER=${ROOTS_AFTER_FILE:-/tmp/space-hygiene-roots-after.txt}
ROOT_TARGETS=${ROOT_TARGETS_FILE:-/tmp/space-hygiene-root-targets.txt}
REACH=${REACH_FILE:-/tmp/space-hygiene-reach.txt}
SERIES=${SERIES_FILE:-/tmp/space-hygiene-root-series.$$.txt}
MEAS=${MEAS_FILE:-/tmp/space-hygiene-meas.$$}
STOPFILE=${STOPFILE:-/tmp/space-hygiene-sampler-stop.$$}
ROOTS_BEFORE_INPUT=${ROOTS_BEFORE_INPUT:-}
BASELINE_FILE=${BASELINE_FILE:-}
BASELINE_AT=${BASELINE_AT:-}
PREWARM_DONE=${PREWARM_DONE:-NO}
WARMUP_REACHABLE=${WARMUP_REACHABLE:-}
T0=$(date +%s)

: >"$OUT"; : >"$MEAS"; : >"$SERIES"

echo "=== anchored pass v5 start $(date -Is) ===" >>"$OUT"

# ------------------------------------------- optional root-count sampler ----
# The retry gate disables this during an accepted measurement bracket so the
# sampler cannot become a concurrent observer of the roots being counted.
SAMPLER_PID=""
SAMPLE_ROOTS=${SAMPLE_ROOTS:-YES}
if [ "$SAMPLE_ROOTS" = YES ]; then
  (
    while [ ! -e "$STOPFILE" ]; do
      n=$(nix-store --gc --print-roots 2>/dev/null | wc -l)
      printf '%s %s\n' "$(date -u +%FT%TZ)" "$n" >>"$SERIES"
      sleep 5
    done
  ) &
  SAMPLER_PID=$!
else
  echo "root_sampler=DISABLED_FOR_ACCEPTANCE_BRACKET" >>"$OUT"
fi

# ---------------------------------------------------------------- warm-up ---
if [ "$PREWARM_DONE" = YES ]; then
  echo "warmup_reachable=${WARMUP_REACHABLE:-not_recorded}" >>"$OUT"
else
  nix-store --gc --print-roots 2>/dev/null | awk '{print $NF}' | grep '^/nix/store/' >"$ROOT_TARGETS"
  xargs -r -d '\n' -n 40 nix-store -qR <"$ROOT_TARGETS" 2>/dev/null | sort -u >"$REACH"
  WARMUP_REACHABLE=$(wc -l <"$REACH")
  echo "warmup_reachable=$WARMUP_REACHABLE" >>"$OUT"
  sleep 10   # let daemon-released transient roots clear before the bracket
fi

# ------------------------------------------------------- roots before ------
if [ -n "$ROOTS_BEFORE_INPUT" ]; then
  cp "$ROOTS_BEFORE_INPUT" "$ROOTS_BEFORE"
else
  nix-store --gc --print-roots 2>/dev/null | sort >"$ROOTS_BEFORE"
fi
ROOTS_BEFORE_COUNT=$(wc -l <"$ROOTS_BEFORE")
PRE_SWEEP_AT=${BASELINE_AT:-$(date -u +%FT%TZ)}
echo "pre_sweep_baseline_at=$PRE_SWEEP_AT" >>"$OUT"
echo "pre_sweep_baseline_count=$ROOTS_BEFORE_COUNT" >>"$OUT"
if [ -n "$BASELINE_FILE" ]; then
  if [ "$ROOTS_BEFORE_INPUT" != "$BASELINE_FILE" ]; then cp "$ROOTS_BEFORE" "$BASELINE_FILE"; fi
  echo "pre_sweep_baseline_file=$BASELINE_FILE" >>"$OUT"
elif [ -n "$ROOTS_BEFORE_INPUT" ]; then
  echo "pre_sweep_baseline_file=$ROOTS_BEFORE_INPUT" >>"$OUT"
fi
echo "roots_before=$ROOTS_BEFORE_COUNT" >>"$OUT"
echo "roots_before_direct=$(grep -vc '^{censored}' "$ROOTS_BEFORE")" >>"$OUT"
echo "roots_before_indirect=$(( ROOTS_BEFORE_COUNT - $(grep -vc '^{censored}' "$ROOTS_BEFORE") ))" >>"$OUT"
echo "store_paths_before=$(ls /nix/store | wc -l)" >>"$OUT"
awk '{print $NF}' "$ROOTS_BEFORE" | grep '^/nix/store/' >"$ROOT_TARGETS"

# ---------------------------------------------------- parallel measurements -
MEAS_PIDS=()
measure() {
  # measure <label> <path>
  local label="$1" path="$2" out
  out=$(du -sx --bytes "$path" 2>/dev/null | awk '{print $1}')
  if [ -z "$out" ]; then
    echo "$label|UNREADABLE|$path" >>"$MEAS"
  else
    echo "$label|$out|$path" >>"$MEAS"
  fi
}

measure area_local_share      "$HOME/.local/share" &
MEAS_PIDS+=($!)
measure area_local_state      "$HOME/.local/state" &
MEAS_PIDS+=($!)
measure area_local_bin        "$HOME/.local/bin" &
MEAS_PIDS+=($!)
measure area_nix_store        /nix/store &
MEAS_PIDS+=($!)
measure area_chat             "$HOME/chat" &
MEAS_PIDS+=($!)
measure area_varlog           /var/log &
MEAS_PIDS+=($!)
measure area_varlog_journal   /var/log/journal &
MEAS_PIDS+=($!)
measure area_containers       "$HOME/.local/share/containers" &
MEAS_PIDS+=($!)
measure area_containers_store "$HOME/.local/share/containers/storage" &
MEAS_PIDS+=($!)
measure area_backups          "$HOME/Backups" &
MEAS_PIDS+=($!)
measure area_dracon_backups   "$HOME/dracon/backups" &
MEAS_PIDS+=($!)
measure agent_codex           "$HOME/.codex" &
MEAS_PIDS+=($!)
measure agent_hermes          "$HOME/.hermes" &
MEAS_PIDS+=($!)
measure agent_opencode_share  "$HOME/.local/share/opencode" &
MEAS_PIDS+=($!)
measure agent_opencode_home   "$HOME/.opencode" &
MEAS_PIDS+=($!)
measure agent_pi              "$HOME/.pi" &
MEAS_PIDS+=($!)
measure agent_zcode           "$HOME/.zcode" &
MEAS_PIDS+=($!)
measure agent_kilocode        "$HOME/.kilocode" &
MEAS_PIDS+=($!)
measure cand_gates_today      "$HOME/.local/state/dracon/sync/gates/browser-extensions-shared" &
MEAS_PIDS+=($!)
measure cand_chat_pi_kimi     "$HOME/chat/pi/kimi" &
MEAS_PIDS+=($!)
measure cand_protection_repair "$HOME/.local/state/dracon/protection-repair-2026-10-20/verification-target" &
MEAS_PIDS+=($!)
measure cand_opencode_snapshot "$HOME/.local/share/opencode/snapshot" &
MEAS_PIDS+=($!)
measure cand_writer_fence     "$HOME/.local/state/dracon-sync/qualification/canonical-writer-fence-2026-10-27" &
MEAS_PIDS+=($!)
measure cand_muse_sessions    "$HOME/.local/share/muse/sessions" &
MEAS_PIDS+=($!)
measure cand_pnpm_store       "$HOME/.local/share/pnpm/store" &
MEAS_PIDS+=($!)
measure cand_mimocode_snapshot "$HOME/.local/share/mimocode/snapshot" &
MEAS_PIDS+=($!)
measure cand_rtk_history      "$HOME/.local/share/rtk/history.db" &
MEAS_PIDS+=($!)
measure cand_trash            "$HOME/.local/share/Trash" &
MEAS_PIDS+=($!)
measure cand_codex_packages   "$HOME/.codex/packages" &
MEAS_PIDS+=($!)
measure cand_codex_sessions   "$HOME/.codex/sessions" &
MEAS_PIDS+=($!)
measure off_videos            "$HOME/Videos" &
MEAS_PIDS+=($!)
measure off_pictures          "$HOME/Pictures" &
MEAS_PIDS+=($!)
measure off_config            "$HOME/.config" &
MEAS_PIDS+=($!)
measure off_dracon            "$HOME/.dracon" &
MEAS_PIDS+=($!)
measure off_nixos_config      "$HOME/.dracon/nixos" &
MEAS_PIDS+=($!)
measure off_etc_nixos         /etc/nixos &
MEAS_PIDS+=($!)

GATES="$HOME/.local/state/dracon/sync/gates/browser-extensions-shared"
(
  find "$GATES" -mindepth 1 -maxdepth 1 -type d -name 'dracon-pre-push.*' ! -newermt '2026-10-28 00:00' -print0 2>/dev/null \
    | xargs -0 -r du -sx --bytes 2>/dev/null | awk -v tag=sandboxes_older_than_2026-10-28 '{s+=$1;n++} END{print tag"|"n"|"s}'
) >>"$MEAS" &
MEAS_PIDS+=($!)
(
  find "$GATES" -mindepth 1 -maxdepth 1 -type d -name 'dracon-pre-push.*' -newermt '2026-10-28 00:00' -print0 2>/dev/null \
    | xargs -0 -r du -sx --bytes 2>/dev/null | awk -v tag=sandboxes_2026-10-28 '{s+=$1;n++} END{print tag"|"n"|"s}'
) >>"$MEAS" &
MEAS_PIDS+=($!)
(
  find "$GATES" -mindepth 1 -maxdepth 1 -type d -name 'dracon-pre-push.*' -print0 2>/dev/null \
    | xargs -0 -r du -sx --bytes 2>/dev/null | awk -v tag=sandboxes_all '{s+=$1;n++} END{print tag"|"n"|"s}'
) >>"$MEAS" &
MEAS_PIDS+=($!)
(
  find "$HOME/Dev" -type d -name .git -prune -exec du -sx --bytes {} + 2>/dev/null \
    | awk -v tag=git_history '{s+=$1;n++} END{printf "%s|%d|%.0f\n", tag, n, s}'
) >>"$MEAS" &
MEAS_PIDS+=($!)

for p in "${MEAS_PIDS[@]}"; do wait "$p"; done
sort "$MEAS" >>"$OUT"
# Preserve the generated measurement scratch file: this sweep is report-only.

# ------------------------------------------------- second closure query -----
xargs -r -d '\n' -n 40 nix-store -qR <"$ROOT_TARGETS" 2>/dev/null | sort -u >"$REACH"
echo "reachable_end=$(wc -l <"$REACH")" >>"$OUT"
sleep 10   # let daemon-released transient roots clear before the bracket

# ---------------------------------------------------------- roots after -----
nix-store --gc --print-roots 2>/dev/null | sort >"$ROOTS_AFTER"
ROOTS_AFTER_COUNT=$(wc -l <"$ROOTS_AFTER")
echo "roots_after=$ROOTS_AFTER_COUNT" >>"$OUT"
echo "roots_after_direct=$(grep -vc '^{censored}' "$ROOTS_AFTER")" >>"$OUT"
echo "roots_after_indirect=$(( ROOTS_AFTER_COUNT - $(grep -vc '^{censored}' "$ROOTS_AFTER") ))" >>"$OUT"
echo "store_paths_after=$(ls /nix/store | wc -l)" >>"$OUT"

# ------------------------------------------------------------ comparison ----
echo "--- comparison ---" >>"$OUT"
if [ "$ROOTS_BEFORE_COUNT" = "$ROOTS_AFTER_COUNT" ]; then
  echo "roots_unchanged=YES" >>"$OUT"
else
  echo "roots_unchanged=NO" >>"$OUT"
fi
if cmp -s "$ROOTS_BEFORE" "$ROOTS_AFTER"; then
  echo "root_set_identical=YES" >>"$OUT"
else
  echo "root_set_identical=NO" >>"$OUT"
  echo "root_set_added:" >>"$OUT"
  comm -13 "$ROOTS_BEFORE" "$ROOTS_AFTER" | sed 's/^/  + /' >>"$OUT"
  echo "root_set_removed:" >>"$OUT"
  comm -23 "$ROOTS_BEFORE" "$ROOTS_AFTER" | sed 's/^/  - /' >>"$OUT"
fi

# ------------------------------------------------------- optional series ----
if [ -n "$SAMPLER_PID" ]; then
  touch "$STOPFILE"
  wait "$SAMPLER_PID" 2>/dev/null
  echo "root_series_samples=$(wc -l <"$SERIES")" >>"$OUT"
  echo "root_series_min=$(awk '{print $2}' "$SERIES" | sort -n | head -1)" >>"$OUT"
  echo "root_series_max=$(awk '{print $2}' "$SERIES" | sort -n | tail -1)" >>"$OUT"
  echo "root_series_distinct=$(awk '{print $2}' "$SERIES" | sort -n | uniq -c | tr '\n' ' ')" >>"$OUT"
  echo "root_series_constant=$( [ "$(awk '{print $2}' "$SERIES" | sort -u | wc -l)" -eq 1 ] && echo YES || echo NO )" >>"$OUT"
  echo "root_series_transitions=$(awk 'NR>1 && $2!=p {c++} {p=$2} END{print c+0}' "$SERIES")" >>"$OUT"
  echo "root_series_first=$(head -1 "$SERIES")" >>"$OUT"
  echo "root_series_last=$(tail -1 "$SERIES")" >>"$OUT"
else
  echo "root_series_samples=0" >>"$OUT"
  echo "root_series_min=NOT_SAMPLED" >>"$OUT"
  echo "root_series_max=NOT_SAMPLED" >>"$OUT"
  echo "root_series_constant=NOT_SAMPLED" >>"$OUT"
  echo "root_series_transitions=NOT_SAMPLED" >>"$OUT"
fi
echo "pass_seconds=$(( $(date +%s) - T0 ))" >>"$OUT"
echo "=== anchored pass v5 end $(date -Is) ===" >>"$OUT"
cat "$OUT"
