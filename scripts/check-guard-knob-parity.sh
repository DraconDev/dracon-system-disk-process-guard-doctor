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
  forgotten_orphan_tree_footprint_mb
)

# BOOLEAN knobs: the same four sources, compared as true/false. They need
# their own pass because the numeric machinery above parses `fn default_x()
# -> u64` and a `knob = <digits>;` assignment, and a boolean is neither.
GUARD_BOOL_KNOBS=(
  reclaim_reaped_browser_profiles
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

# A knob's value in the NixOS operator module: the Nix attribute
# `services.dracon.system.policy` assigns it, e.g.
# `tmp_large_entry_bytes = 1073741824;` or
# `reclaim_reaped_browser_profiles = true;`.
#
# EVERY match is printed: a knob assigned in two places in the module would
# make "the module's value" ambiguous, and the caller rejects that.
flake_values() {
  local knob="$1"
  [ -f "$PARENT_FLAKE" ] || return 1
  awk -v knob="$knob" '
    $0 ~ "^[[:space:]]*" knob "[[:space:]]*=[[:space:]]*([0-9]+|true|false)[[:space:]]*;" {
      line = $0; sub(/^[^=]*=[[:space:]]*/, "", line); gsub(/[[:space:];].*$/, "", line);
      print line
    }
  ' "$PARENT_FLAKE"
}

# A BOOLEAN knob's crate default: the `default_true` / `default_false`
# function its serde attribute names.
crate_bool_default() {
  local knob="$1"
  local fn
  fn="$(awk -v knob="$knob" '
    /#\[serde\(default = "default_[a-z_]+"\)\]/ { pending = $0 }
    $0 ~ "pub\\(crate\\) " knob ": bool," { if (pending != "") { print pending; pending = "" } }
  ' "$POLICY_RS" | grep -oE 'default_[a-z_]+' | head -n1)"
  case "$fn" in
    default_true) echo "true" ;;
    default_false) echo "false" ;;
    *) return 1 ;;
  esac
}

echo "• guard knobs: crate defaults vs the NixOS module vs the example policy vs the live policy"
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
  flake_vals="$(flake_values "$knob" || true)"
  flake_count="$(printf '%s' "$flake_vals" | grep -c . || true)"
  flake_val="$(printf '%s' "$flake_vals" | head -n1)"
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
  if [ "$flake_count" -eq 0 ]; then
    echo "✗ $knob: not declared by the NixOS operator module ($PARENT_FLAKE)" >&2
    fail=1
    continue
  fi
  if [ "$flake_count" -gt 1 ]; then
    echo "✗ $knob: declared $flake_count times in the NixOS operator module — the module's value would be ambiguous" >&2
    fail=1
    continue
  fi
  if [ "$crate_val" != "$example_val" ] || [ "$crate_val" != "$live_val" ] || [ "$crate_val" != "$flake_val" ]; then
    echo "✗ $knob: crate default $crate_val, NixOS module $flake_val, example $example_val, live $live_val" >&2
    fail=1
    continue
  fi
  echo "  $knob = $crate_val (crate default, NixOS module, example, live)"
done

# The same four sources for the BOOLEAN knobs.
echo "• guard boolean knobs: crate defaults vs the NixOS module vs the example policy vs the live policy"
for knob in "${GUARD_BOOL_KNOBS[@]}"; do
  crate_val="$(crate_bool_default "$knob" || true)"
  example_val="$(toml_value "$EXAMPLE_TOML" "$knob" || true)"
  live_val="$(toml_value "$LIVE_TOML" "$knob" || true)"
  flake_vals="$(flake_values "$knob" || true)"
  flake_count="$(printf '%s' "$flake_vals" | grep -c . || true)"
  flake_val="$(printf '%s' "$flake_vals" | head -n1)"
  if [ -z "$crate_val" ]; then
    echo "✗ $knob: no boolean crate default found in src/policy.rs" >&2
    fail=1
    continue
  fi
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
  if [ "$flake_count" -eq 0 ]; then
    echo "✗ $knob: not declared by the NixOS operator module ($PARENT_FLAKE)" >&2
    fail=1
    continue
  fi
  if [ "$flake_count" -gt 1 ]; then
    echo "✗ $knob: declared $flake_count times in the NixOS operator module — the module's value would be ambiguous" >&2
    fail=1
    continue
  fi
  if [ "$crate_val" != "$example_val" ] || [ "$crate_val" != "$live_val" ] || [ "$crate_val" != "$flake_val" ]; then
    echo "✗ $knob: crate default $crate_val, NixOS module $flake_val, example $example_val, live $live_val" >&2
    fail=1
    continue
  fi
  echo "  $knob = $crate_val (crate default, NixOS module, example, live)"
done

# The module DECLARES the knobs but must not WRITE the operator's policy
# file by default: that file is the operator's own, and a module that
# overwrote it silently would be exactly the drift this check exists to
# catch. An operator who wants Nix to own the file opts in explicitly.
echo "• the NixOS module: it declares the knobs but does not take the operator's policy file by default"
if [ -f "$PARENT_FLAKE" ]; then
  if grep -qE "^[[:space:]]*system\.managePolicy = mkOption" "$PARENT_FLAKE"; then
    manage_default="$(awk '/^[[:space:]]*system\.managePolicy = mkOption/ { inside = 1; next } inside && /default = / { gsub(/.*default = /, ""); gsub(/;.*/, ""); print; exit }' "$PARENT_FLAKE")"
    if [ "$manage_default" = "false" ]; then
      echo "  services.dracon.system.managePolicy defaults to false (the operator's policy file is not overwritten)"
    else
      echo "✗ services.dracon.system.managePolicy must default to false (the operator's policy file must not be overwritten silently)" >&2
      fail=1
    fi
  else
    echo "✗ the NixOS module does not declare services.dracon.system.managePolicy — the operator's policy file could be overwritten without a visible switch" >&2
    fail=1
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
