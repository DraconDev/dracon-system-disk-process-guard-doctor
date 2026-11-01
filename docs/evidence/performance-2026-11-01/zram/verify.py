#!/usr/bin/env python3
"""Check stored evidence integrity and reproducibility, without new host reads."""
import datetime
import hashlib
import json
from pathlib import Path
import re
import analyse as a

OUT = Path(__file__).resolve().parent
ROOT = OUT.parents[3]


def main():
    host = json.loads((OUT / 'host.json').read_text())
    derived = json.loads((OUT / 'derived.json').read_text())
    for sample in host['samples']:
        for path, read in sample['reads'].items():
            start = datetime.datetime.fromisoformat(read['started_at'])
            end = datetime.datetime.fromisoformat(read['finished_at'])
            assert start <= end and read['monotonic_ns'] <= read['finished_monotonic_ns'], path
            if read['text'] is not None:
                assert hashlib.sha256(read['text'].encode()).hexdigest() == read['sha256'], path
    assert derived['frames'] == [a.frame(s, '/sys/block/zram0') for s in host['samples']]
    assert derived['intervals'] == [a.interval(x,y,host['page_size_bytes']) for x,y in zip(host['samples'],host['samples'][1:])]
    report = OUT.parent / 'zram-assessment.md'
    for link in re.findall(r'\[[^\]]+\]\(([^)]+)\)', report.read_text()):
        if not link.startswith(('https://','http://')):
            assert (report.parent / link.split('#')[0]).is_file(), link
    commands = json.loads((OUT / 'commands.json').read_text())
    assert commands['capture']['exit'] == commands['status']['exit'] == 0
    assert hashlib.sha256((ROOT / 'src/zram.rs').read_bytes()).hexdigest() == commands['production_zram_source_sha256']
    print('PASS: read hashes/timestamps, reproducible frame/rate calculations, local citations, captured exit statuses and unchanged production zram source')


if __name__ == '__main__':
    main()
