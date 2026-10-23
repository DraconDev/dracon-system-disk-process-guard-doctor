#!/usr/bin/env python3
"""Replicate the guard's forgotten-orphan scan against the live /proc.

Read-only probe: it mirrors src/forgotten.rs + src/reap.rs logic exactly so
we can measure the real blind spot on this host, and also measure the
AGGREGATE footprint of each orphaned browser tree (which the guard does not
compute today).
"""
import os, re, sys, json

PROC = "/proc"
HERTZ = 100

# --- live policy values (mirrored from ~/.dracon/utilities/system/dracon-system.toml)
EXEMPT = ["systemd", "dbus-daemon", "Xorg", "kwin_wayland", "plasmashell",
          "dracon-code", "pi", "wezterm", "wezterm-gui", "chrome"]
MAX_CPU_SECONDS = 60
FOOTPRINT_FLOOR_MB = 2048
SIGNATURES = ["eve web", "vite", "playwright", "chromium", "bun run", "bun test",
              "npm run test", ".mjs", "litestream", "stub-auth-api"]

HEADLESS_MARKERS = ["--headless", "headless_shell"]
TEST_PROFILE_MARKERS = ["playwright", "puppeteer", "chromium-", "chrome-",
                        "headless", ".cache", "tmp"]


def parse_stat(pid):
    try:
        with open(f"{PROC}/{pid}/stat", "rb") as f:
            raw = f.read().decode("utf-8", "replace")
    except OSError:
        return None
    if ")" not in raw:
        return None
    tail = raw.rsplit(")", 1)[1].split()
    if len(tail) < 20:
        return None
    try:
        return {
            "state": tail[0][0],
            "ppid": int(tail[1]),
            "tty_nr": int(tail[4]),
            "cpu_ticks": int(tail[11]) + int(tail[12]),
            "starttime": int(tail[19]),
        }
    except (ValueError, IndexError):
        return None


def comm_of(pid):
    try:
        with open(f"{PROC}/{pid}/comm") as f:
            return f.read().strip()
    except OSError:
        return None


def cmdline_of(pid):
    try:
        with open(f"{PROC}/{pid}/cmdline", "rb") as f:
            return f.read().decode("utf-8", "replace").replace("\0", " ").strip()
    except OSError:
        return None


def rss_mb(pid):
    try:
        with open(f"{PROC}/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) // 1024
    except OSError:
        pass
    return 0


def swap_mb(pid):
    try:
        with open(f"{PROC}/{pid}/status") as f:
            for line in f:
                if line.startswith("VmSwap:"):
                    return int(line.split()[1]) // 1024
    except OSError:
        pass
    return 0


def classify(args):
    """Mirror forgotten::classify_desktop_session."""
    lower = args.lower()
    if "chrom" not in lower:
        return ("NotABrowser", None)
    if any(m in lower for m in HEADLESS_MARKERS):
        return ("Test", "headless")
    idx = lower.find("--user-data-dir=")
    if idx == -1:
        return ("Desktop", "no --user-data-dir: a normal browser session")
    rest = lower[idx + len("--user-data-dir="):]
    end = len(rest)
    for i, c in enumerate(rest):
        if c.isspace() or c in "\"'":
            end = i
            break
    path = rest[:end].strip()
    if not path:
        return ("Desktop", "no --user-data-dir: a normal browser session")
    if any(m in path for m in TEST_PROFILE_MARKERS):
        return ("Test", f"test profile {path}")
    return ("Desktop", f"profile {path} is not a test profile")


def main():
    pids = []
    for name in os.listdir(PROC):
        if name.isdigit():
            pids.append(int(name))

    stats, kids = {}, {}
    for pid in pids:
        st = parse_stat(pid)
        if not st:
            continue
        stats[pid] = st
        kids.setdefault(st["ppid"], []).append(pid)

    exempted_by = {}
    rows = []
    for pid, st in stats.items():
        if pid <= 1:
            continue
        if comm_of(st["ppid"]) != "systemd":
            continue          # not an orphan
        if st["tty_nr"] != 0:
            continue
        if st["state"] != "S":
            continue
        args = cmdline_of(pid)
        if not args:
            continue
        hit = next((e for e in EXEMPT if e in args), None)
        if hit:
            exempted_by.setdefault(hit, []).append(pid)
            continue
        if st["cpu_ticks"] // HERTZ > MAX_CPU_SECONDS:
            continue
        fp = rss_mb(pid) + swap_mb(pid)
        verdict, why = classify(args)
        rows.append({"pid": pid, "fp": fp, "verdict": verdict, "why": why,
                     "comm": comm_of(pid) or "", "args": args[:160]})

    # aggregate tree footprint: walk up to the orphan root
    def root_of(pid):
        seen = set()
        cur = pid
        while cur not in seen:
            seen.add(cur)
            st = stats.get(cur)
            if not st:
                return pid
            if comm_of(st["ppid"]) == "systemd":
                return cur
            cur = st["ppid"]
        return pid

    trees = {}
    for r in rows:
        root = root_of(r["pid"])
        t = trees.setdefault(root, {"pid": root, "fp": 0, "members": 0,
                                    "verdicts": set(), "why": None, "comm": r["comm"]})
        t["fp"] += r["fp"]
        t["members"] += 1
        t["verdicts"].add(r["verdict"])
        if r["verdict"] == "Test" and t["why"] is None:
            t["why"] = r["why"]

    print("=" * 78)
    print("A. WHAT THE GUARD SEES TODAY (forgotten scan, live policy)")
    print("=" * 78)
    print(f"processes exempt by the shared exempt list: {sum(len(v) for v in exempted_by.values())}")
    for k, v in sorted(exempted_by.items(), key=lambda kv: -len(kv[1])):
        print(f"    exempt token {k!r:22} -> {len(v)} process(es)")
    test_rows = [r for r in rows if r["verdict"] == "Test"]
    desk_rows = [r for r in rows if r["verdict"] == "Desktop"]
    print(f"\norphans passing every gate except weight: {len(rows)}")
    print(f"    classified TEST      : {len(test_rows)}")
    print(f"    classified DESKTOP   : {len(desk_rows)}")
    above = [r for r in test_rows if r["fp"] >= FOOTPRINT_FLOOR_MB]
    print(f"    TEST and ABOVE the {FOOTPRINT_FLOOR_MB} MiB per-process floor: {len(above)}")
    print(f"    TEST but BELOW the floor (invisible today): {len(test_rows) - len(above)}")

    print()
    print("=" * 78)
    print("B. AGGREGATE TREE FOOTPRINT (what the guard does NOT compute)")
    print("=" * 78)
    print(f"{'root pid':>10} {'members':>8} {'tree MiB':>10}  verdict  evidence")
    total = 0
    agg_kills = 0
    for t in sorted(trees.values(), key=lambda t: -t["fp"]):
        if not t["fp"]:
            continue
        v = "/".join(sorted(t["verdicts"]))
        marker = ""
        if "Test" in t["verdicts"] and t["fp"] >= FOOTPRINT_FLOOR_MB:
            marker = "  <== AGGREGATE OVER FLOOR"
            agg_kills += 1
        total += t["fp"]
        print(f"{t['pid']:>10} {t['members']:>8} {t['fp']:>10}  {v:<16} {(t['why'] or '')[:38]}{marker}")
    print(f"\ntrees whose AGGREGATE crosses {FOOTPRINT_FLOOR_MB} MiB: {agg_kills}")
    print(f"total footprint across all orphan trees: {total} MiB")

    print()
    print("=" * 78)
    print("C. EXEMPT-LIST BLIND SPOT: are test browsers exempt as collateral?")
    print("=" * 78)
    # show exempt orphan browsers and why they matched
    for token, plist in sorted(exempted_by.items(), key=lambda kv: -len(kv[1])):
        sample = []
        for pid in plist[:400]:
            args = cmdline_of(pid) or ""
            v, why = classify(args)
            if v == "Test":
                sample.append((pid, why))
        if sample:
            print(f"exempt token {token!r} matched {len(sample)} TEST-job browser process(es) "
                  f"-> the forgotten pass never sees them")
            for pid, why in sample[:3]:
                a = (cmdline_of(pid) or "")[:150]
                print(f"      pid {pid}: {why}")
                print(f"           {a}")


if __name__ == "__main__":
    main()
