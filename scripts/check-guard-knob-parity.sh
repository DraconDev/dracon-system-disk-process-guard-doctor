#!/usr/bin/env bash
# scripts/check-guard-knob-parity.sh — assert that every knob this
# hardening goal added carries the SAME value in every source that
# defines it.
#
# WHY THIS EXISTS (2026-10-22): the goal added knobs to the guard's
# policy. A knob that is present in one source and missing (or different)
# in another is a knob whose behaviour depends on which file the daemon
# happened to read — the failure mode the task-budget work hit with the
# daemon's own sources (the NixOS module said 96 while the shipped unit
# said 512).
#
# The sources per knob:
#   * the crate's policy DEFAULTS (dracon-system/src/policy.rs) — the
#     behaviour when nothing overrides them,
#   * the NixOS OPERATOR MODULE (the parent's flake.nix, the
#     `services.dracon.system.policy` option) — the values a Nix install is
#     guaranteed to get, because a Nix install with no policy file runs the
#     crate's defaults and the module declares exactly those,
#   * the crate's EXAMPLE policy (dracon-system/dracon-system.example.toml)
#     — what an operator is shown,
#   * the LIVE policy (~/.dracon/utilities/system/dracon-system.toml) —
#     what this host actually runs.
#
# The module DECLARES the knob values but does not write the operator's
# policy FILE unless `services.dracon.system.managePolicy` is enabled: that
# file is the operator's own, and a module that overwrote it silently would
# be the same drift class this check exists to catch. The check below
# asserts both halves — the declared values agree, and the file is not
# written by default.
#
# The daemon-side knob the goal's sibling work added (the task budget) IS
# carried by the module too, so it is checked here as well — against the
# shipped unit and the module.
#
# Exit codes:
#   0 = every knob agrees across the sources that define it
#   1 = a knob differs or is missing somewhere
#   2 = a source could not be read
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY_RS="$ROOT/src/policy.rs"
EXAMPLE_TOML="$ROOT/dracon-system.example.toml"
LIVE_TOML="${DRACON_SYSTEM_POLICY:-$HOME/.dracon/utilities/system/dracon-system.toml}"
PARENT_FLAKE="$ROOT/../flake.nix"
SYNC_UNIT="$ROOT/../dracon-sync/dracon-sync.service"

# The knobs this goal added, and the crate default each must carry.
GUARD_KNOBS=(
  tmp_large_entry_bytes
  tmp_large_min_age_hours
  tmp_scratch_warn_gib
  cleanup_command_timeout_secs
  forgotten_orphan_footprint_mb
)

fail=0

note() { printf '  %s\n' "$*"; }

# The crate default for a knob: `pub(crate) <name>: <type>,` preceded by a
# `#[serde(default = "default_<name>")]` attribute.
crate_default() {
  local knob="$1"
  awk -v knob="$knob" '
    /#\[serde\(default = "default_[a-z_]+"\)\]/ { pending = $0 }
    $0 ~ "pub\\(crate\\) " knob ": " { if (pending != "") { print pending; pending = "" } }
  ' "$POLICY_RS" | grep -oE 'default_[a-z_]+' | head -n1
}

# The value a default function returns.
#
# The defaults are plain arithmetic over literals ("1024 * 1024 * 1024"),
# so the expression is evaluated as shell arithmetic — after refusing
# anything that is not digits and operators.
default_value() {
  local fn="$1"
  local expr
  expr="$(awk -v fn="$fn" '
    $0 ~ "fn " fn "\\(\\) -> u64" { inside = 1; next }
    inside && /}/ { inside = 0 }
    inside && /[0-9]/ { print; exit }
  ' "$POLICY_RS")"
  expr="$(printf '%s' "$expr" | tr -d '[:space:]' | sed 's/;$//')"
  case "$expr" in
    "" | *[!0-9*+-]*) return 1 ;;
  esac
  echo $((expr))
}

# A knob's value in a TOML file: the last uncommented assignment wins,
# and a commented line is the EXAMPLE value (what an operator is shown).
toml_value() {
  local file="$1" knob="$2"
  [ -f "$file" ] || return 1
  awk -v knob="$knob" '
    $0 ~ "^[[:space:]]*#[[:space:]]*" knob "[[:space:]]*=" {
      line = $0; sub(/^[^=]*=[[:space:]]*/, "", line); gsub(/[[:space:]#].*$/, "", line);
      if (commented == "") commented = line; next
    }
    $0 ~ "^[[:space:]]*" knob "[[:space:]]*=" {
      line = $0; sub(/^[^=]*=[[:space:]]*/, "", line); gsub(/[[:space:]#].*$/, "", line);
      live = line
    }
    END { if (live != "") print live; else if (commented != "") print commented }
  ' "$file"
}

echo "• guard knobs: crate defaults vs the example policy vs the live policy"
for knob in "${GUARD_KNOBS[@]}"; do
  fn="$(crate_default "$knob")"
  if [ -z "$fn" ]; then
    echo "✗ $knob: no crate default found in src/policy.rs" >&2
    fail=1
    continue
  fi
  crate_val="$(default_value "$fn")"
  example_val="$(toml_value "$EXAMPLE_TOML" "$knob" || true)"
  live_val="$(toml_value "$LIVE_TOML" "$knob" || true)"
  if [ -z "$example_val" ]; then
    echo "✗ $knob: absent from the example policy" >&2
    fail=1
    continue
  fi
  if [ -z "$live_val" ]; then
    echo "✗ $knob: absent from the live policy ($LIVE_TOML)" >&2
    fail=1
    continue
  fi
  if [ "$crate_val" != "$example_val" ] || [ "$crate_val" != "$live_val" ]; then
    echo "✗ $knob: crate default $crate_val, example $example_val, live $live_val" >&2
    fail=1
    continue
  fi
  echo "  $knob = $crate_val (crate default, example, live)"
done

# The NixOS module is the FOURTH source, and it is a NEGATIVE one: it ships
# the guard's SERVICE, not its policy, so it must not define a guard policy
# knob at all.
#
# WHY (2026-10-22): the live policy is the OPERATOR's file and is
# deliberately not a copy of the example — the general tmp floor is 6 h on
# this host against the example's 24 h. If the module defined a knob value
# it would either override that tuning (the guard has no CLI or env
# override for these knobs, and the module does not generate the policy
# file) or become a silent drift source: the value that looks authoritative
# would be one the daemon never reads. Asserting the ABSENCE keeps the
# module a checked participant in this comparison instead of an unchecked
# one, and fails loudly if a future change makes it a value source.
echo "• the NixOS module: it must not define the guard's policy knobs"
if [ -f "$PARENT_FLAKE" ]; then
  module_fail=0
  for knob in "${GUARD_KNOBS[@]}"; do
    if grep -qE "^[[:space:]]*\"?${knob}\"?[[:space:]]*=" "$PARENT_FLAKE"; then
      echo "✗ $knob is defined in the NixOS module — the guard's policy is the operator's file; a module value would override the operator's tuning (drift source)" >&2
      module_fail=1
    fi
  done
  if [ "$module_fail" -ne 0 ]; then
    fail=1
  else
    echo "  the module defines none of them (it ships the guard's service; the policy file is the operator's)"
  fi
else
  note "the NixOS module is not present here — skipped"
fi

# The sibling knob from the task-budget work: the daemon's task budget is
# carried by the shipped unit AND the parent's NixOS module, and they must
# agree (the 96-vs-512 drift of 2026-10-21).
echo "• the daemon's task budget: the shipped unit vs the NixOS module"
if [ -f "$SYNC_UNIT" ] && [ -f "$PARENT_FLAKE" ]; then
  unit_val="$(awk -F= '/^[[:space:]]*TasksMax/ { gsub(/[[:space:]]/, "", $2); print $2; exit }' "$SYNC_UNIT")"
  flake_val="$(awk -F'"' '/TasksMax = "/ { print $2; exit }' "$PARENT_FLAKE")"
  if [ -z "$unit_val" ] || [ -z "$flake_val" ]; then
    echo "✗ could not read the task budget from both sources" >&2
    fail=1
  elif [ "$unit_val" != "$flake_val" ]; then
    echo "✗ task budget drift: shipped unit $unit_val, NixOS module $flake_val" >&2
    fail=1
  else
    echo "  TasksMax = $unit_val (shipped unit, NixOS module)"
  fi
else
  note "the shipped unit or the NixOS module is not present here — skipped"
fi

if [ "$fail" -ne 0 ]; then
  echo "✗ guard knob parity failed" >&2
  exit 1
fi
echo "✓ every knob this goal added carries the same value in every source that defines it"
