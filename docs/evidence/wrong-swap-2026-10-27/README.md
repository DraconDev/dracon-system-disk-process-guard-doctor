# Wrong-swap check — verification evidence (2026-10-27)

Objective: the guard verifies on every pass that all ACTIVE swap lives on an
expected device (zram + /mnt/data by default) and that on-disk swapfiles are
referenced by the NixOS `swapDevices` config. Surfaced in `doctor`, the disk
audit REPORT section, `guard once`, and the daemon event stream. Never
auto-deleted.

## Transcripts (reproducible on this host)

- `doctor.txt` / `doctor.json` — live `doctor`: `swap devices ok`,
  `swap_probe_available: true, swap_ok: true`.
  Active swap on this host: `/dev/zram0` + `/mnt/data/swapfile`, both
  referenced in `~/.dracon/nixos/configuration.nix` (`swapDevices =
  lib.mkForce [...]`).
- `audit-wrong-placement.txt` — forced policy (`expected_swap_prefixes =
  "/nonexistent"` via `DRACON_SYSTEM_POLICY`): both active devices render
  as REPORT with sizes and no removal command.
- `audit-unreferenced.txt` — forced policy (config with empty
  `swapDevices`): `/mnt/data/swapfile` renders as unreferenced-ACTIVE
  REPORT with no removal command.

Repro commands (from the repo root, release binary built):

```
./target/release/dracon-system doctor
printf '[guard]\nexpected_swap_prefixes = "/nonexistent"\n' > /tmp/st.toml
DRACON_SYSTEM_POLICY=/tmp/st.toml ./target/release/dracon-system disk audit --class tmp
```

## Falsification fixes (second pass, same day)

1. `under_prefix` enforced component/device boundaries — `/mnt/database`
   and `/mnt/data2` no longer satisfy `/mnt/data`; `/dev/zram` still
   covers `zram0..N` (test: `prefix_matching_respects_component_boundaries`).
2. `code_only` blanks per byte, not per char — non-ASCII configs keep exact
   byte offsets, no mis-parse or slicing panic
   (test: `unicode_before_block_parses_and_never_panics`).
3. The daemon pass emits: `check_swap_devices` notifies once on entry into
   the wrong state and posts `system/swap-devices` to the event stream at
   `report_repeat_secs` (test: `swap_devices_check_records_its_state_every_pass`).
4. `cmd_guard_clean` tilde-expands swap candidates like every other path.
