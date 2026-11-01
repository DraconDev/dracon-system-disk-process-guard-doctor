#!/usr/bin/env python3
"""Bounded read-only host capture. Writes only its explicit evidence output.
No device/sysctl/configuration writes, pressure workload, or process controls.
"""
import datetime
import hashlib
import json
import os
from pathlib import Path
import time

OUT = Path(__file__).resolve().parent
LIMIT = 262144
PROC = ["/proc/sys/kernel/random/boot_id", "/proc/meminfo", "/proc/swaps", "/proc/vmstat", "/proc/stat", "/proc/diskstats", "/proc/pressure/memory", "/proc/pressure/io", "/proc/pressure/cpu", "/proc/sys/vm/swappiness", "/proc/sys/vm/page-cluster"]
DEVICE = ["dev", "initstate", "disksize", "mm_stat", "comp_algorithm", "mem_limit", "backing_dev", "bd_stat", "io_stat", "stat"]


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def read(path):
    row = dict(path=str(path), started_at=stamp(), monotonic_ns=time.monotonic_ns())
    try:
        with open(path, "rb") as f:
            raw = f.read(LIMIT + 1)
        if len(raw) > LIMIT:
            raise ValueError("capture byte limit exceeded")
        row.update(text=raw.decode("utf-8"), sha256=hashlib.sha256(raw).hexdigest(), error=None)
    except (OSError, ValueError) as exc:
        row.update(text=None, error=f"{type(exc).__name__}: {exc}")
    row.update(finished_at=stamp(), finished_monotonic_ns=time.monotonic_ns())
    return row


def main():
    devices = sorted(Path("/sys/block").glob("zram*"))[:4]
    samples = []
    for i in range(3):
        if i:
            time.sleep(5)
        paths = [Path(p) for p in PROC] + [d / name for d in devices for name in DEVICE]
        samples.append(dict(started_at=stamp(), reads={str(p): read(p) for p in paths}, finished_at=stamp()))
    result = dict(schema=1, page_size_bytes=os.sysconf("SC_PAGE_SIZE"),
                  source="read-only proc/sysfs; sequential not atomic; three observations five seconds apart",
                  byte_limit_per_read=LIMIT, samples=samples)
    (OUT / "host.json").write_text(json.dumps(result, indent=2) + "\n")
    print("Captured", len(samples), "observations of", len(devices), "zram device(s); see host.json")


if __name__ == "__main__":
    main()
