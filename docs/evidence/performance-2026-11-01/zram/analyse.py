#!/usr/bin/env python3
"""Evidence-only calculations, not a replacement for production zram status.
Unknowns remain None; positional fields never shift past malformed tokens.
"""
import json
from pathlib import Path
import re

GIB = 1024 ** 3
U64 = 2 ** 64 - 1
OUT = Path(__file__).resolve().parent


def uint(value: str | None) -> int | None:
    if value is None or not re.fullmatch(r"[0-9]+", value.strip()):
        return None
    result = int(value)
    return result if result <= U64 else None


def scalar(text: str | None) -> int | None:
    return uint(text)


def mm_stat(text: str | None) -> dict:
    tokens = (text or "").split()
    names = ["original_bytes", "compressed_bytes", "ram_bytes"]
    result = {name: uint(tokens[i]) if i < len(tokens) else None for i, name in enumerate(names)}
    result["ram_high_water_bytes"] = uint(tokens[4]) if len(tokens) > 4 else None
    result["same_pages"] = uint(tokens[5]) if len(tokens) > 5 else None
    return result


def ratio(a: int | None, b: int | None) -> float | None:
    return a / b if a is not None and b is not None and b > 0 else None


def memory(text: str | None) -> dict:
    result = {}
    for line in (text or "").splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[0].endswith(":") and fields[2] == "kB":
            value = uint(fields[1])
            name = fields[0][:-1]
            result[name] = value * 1024 if value is not None and value <= U64 // 1024 and name not in result else None
    return result


def counters(text: str | None) -> dict:
    result = {}
    for line in (text or "").splitlines():
        fields = line.split()
        if fields:
            key = fields[0]
            result[key] = uint(fields[1]) if len(fields) == 2 and key not in result else None
    return result


def swaps(text: str | None) -> list | None:
    if text is None or not text.splitlines() or text.splitlines()[0].split() != ["Filename", "Type", "Size", "Used", "Priority"]:
        return None
    result = []
    for line in text.splitlines()[1:]:
        fields = line.split()
        if len(fields) != 5:
            return None
        size, used = uint(fields[2]), uint(fields[3])
        if size is None or used is None or used > size or size > U64 // 1024 or not re.fullmatch(r"-?[0-9]+", fields[4]):
            return None
        result.append(dict(path=fields[0], kind=fields[1], capacity_bytes=size * 1024, allocated_bytes=used * 1024, priority=int(fields[4])))
    if len({r['path'] for r in result}) != len(result):
        return None
    return result


def psi(text: str | None) -> dict:
    result = {}
    for line in (text or "").splitlines():
        fields = line.split()
        if not fields or fields[0] not in ["some", "full"]:
            continue
        label = fields[0]
        values = {}
        for pair in fields[1:]:
            if "=" in pair:
                key, value = pair.split("=", 1)
                values[key] = None if key in values else value
        avg = values.get("avg10")
        total = uint(values.get("total"))
        valid_avg = float(avg) if avg and re.fullmatch(r"[0-9]+(?:\.[0-9]+)?", avg) else None
        if valid_avg is not None and not 0 <= valid_avg <= 100:
            valid_avg = None
        result[label] = dict(avg10_percent=valid_avg, total_microseconds=total) if label not in result else dict(avg10_percent=None, total_microseconds=None)
    return result


def rate(before: int | None, after: int | None, elapsed: float, continuous: bool = True) -> float | None:
    if not continuous or before is None or after is None or after < before or elapsed <= 0:
        return None
    return (after - before) / elapsed


def raw(sample: dict, path: str) -> str | None:
    record = sample['reads'].get(path, {})
    return record.get('text') if not record.get('error') else None


def elapsed(a: dict, b: dict, path: str) -> float:
    return (b['reads'][path]['monotonic_ns'] - a['reads'][path]['monotonic_ns']) / 1e9


def frame(sample: dict, path: str) -> dict:
    stats = mm_stat(raw(sample, path + '/mm_stat'))
    size = scalar(raw(sample, path + '/disksize'))
    result = dict(started_at=sample['started_at'], device=path, logical_capacity_bytes=size, **stats,
                  compression_original_per_payload=ratio(stats['original_bytes'], stats['compressed_bytes']),
                  effective_original_per_allocated_ram=ratio(stats['original_bytes'], stats['ram_bytes']),
                  ram_fraction_of_logical_capacity=ratio(stats['ram_bytes'], size),
                  original_fraction_of_logical_capacity=ratio(stats['original_bytes'], size),
                  meminfo_bytes=memory(raw(sample, '/proc/meminfo')), swap_entries=swaps(raw(sample, '/proc/swaps')),
                  memory_psi=psi(raw(sample, '/proc/pressure/memory')), io_psi=psi(raw(sample, '/proc/pressure/io')))
    return result


def interval(a: dict, b: dict, page_size: int) -> dict:
    boot_a, boot_b = raw(a, '/proc/sys/kernel/random/boot_id'), raw(b, '/proc/sys/kernel/random/boot_id')
    continuous = bool(boot_a and boot_a.strip() and boot_a == boot_b)
    t = elapsed(a, b, '/proc/vmstat')
    x, y = counters(raw(a, '/proc/vmstat')), counters(raw(b, '/proc/vmstat'))
    result = dict(from_timestamp=a['reads']['/proc/vmstat']['started_at'], to_timestamp=b['reads']['/proc/vmstat']['started_at'], elapsed_seconds=t, boot_continuity_observed=continuous)
    for key in ['pswpin', 'pswpout', 'pgmajfault']:
        r = rate(x.get(key), y.get(key), t, continuous)
        result[key + '_per_second'] = r
        if key != 'pgmajfault':
            result[key + '_mib_per_second'] = r * page_size / 1024 ** 2 if r is not None and page_size > 0 else None
    for resource in ['memory', 'io']:
        path = '/proc/pressure/' + resource
        p, q = psi(raw(a, path)), psi(raw(b, path))
        r = rate(p.get('full', {}).get('total_microseconds'), q.get('full', {}).get('total_microseconds'), elapsed(a, b, path), continuous)
        result[resource + '_full_stall_interval_percent'] = r / 1e6 * 100 if r is not None else None
    return result


def main():
    host = json.loads((OUT / 'host.json').read_text())
    path = '/sys/block/zram0'
    samples = host['samples']
    historical = json.loads((OUT.parent / 'raw/export-observations.json').read_text())
    entry = next(r for r in historical['observations'] if r['entry_index'] == 27)
    historical_stats = mm_stat(entry['text'].split('raw mm_stat: ', 1)[1].splitlines()[0])
    result = dict(units='bytes; binary GiB=1024^3; rates pswp base-pages/s and MiB/s; PSI percent',
                  page_size_bytes=host['page_size_bytes'],
                  incident=dict(timestamp=entry['timestamp'], source_entry=27, **historical_stats,
                                compression_original_per_payload=ratio(historical_stats['original_bytes'], historical_stats['compressed_bytes']),
                                effective_original_per_allocated_ram=ratio(historical_stats['original_bytes'], historical_stats['ram_bytes'])),
                  frames=[frame(s, path) for s in samples],
                  intervals=[interval(a, b, host['page_size_bytes']) for a,b in zip(samples, samples[1:])])
    (OUT / 'derived.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
