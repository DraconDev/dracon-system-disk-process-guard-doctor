#!/usr/bin/env bash
# Shipped by dracon-utilities (audit M8, 2026-10-02): previously live-only.
# install.sh copies this to ~/.dracon/system-notify/ (see the watchdog .service).
# dracon-system-guard watchdog — restart the guard daemon if it was stopped.
#
# Rationale (2026-08-10): the 2026-08-09/10 incidents (swap thrash,
# ENOSPC Chrome crash) happened while dracon-system-guard.service was
# DISABLED and INACTIVE — no guard daemon was watching, so nothing
# caught the disk filling or the memory pressure. `Restart=always` in
# the service unit only covers crashes, not manual stops or a disabled
# service. This watchdog mechanically guarantees the guard can never
# stay down: if the service is inactive and no maintenance hold is
# present, it is restarted within ~2.5 minutes (timer period + jitter).
#
# Escape hatch for genuine downtime (guard release installs, hardware
# work, deliberate disable):
#   touch ~/.dracon/dracon-system.maintenance-hold
# The watchdog then skips restart until the marker is removed.
# NOTE: nothing removes the marker automatically — remove it after
# the maintenance window ends.

set -u

HOLD="$HOME/.dracon/dracon-system.maintenance-hold"

if [ -f "$HOLD" ]; then
    echo "dracon-system-guard-watchdog: maintenance hold present ($HOLD) — skipping restart"
    exit 0
fi

if systemctl --user is-active --quiet dracon-system-guard.service; then
    exit 0
fi

echo "dracon-system-guard-watchdog: dracon-system-guard.service is inactive — restarting"
systemctl --user start dracon-system-guard.service
