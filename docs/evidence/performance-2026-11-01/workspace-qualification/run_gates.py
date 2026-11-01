#!/usr/bin/env python3
"""Run exact qualification commands only in the retained frozen parent workspace.
Not collected by any suite; no test invokes this orchestrator. No install,
service-control, live-policy or reclamation operations. Run phases sequentially.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

OUT = Path(__file__).resolve().parent
COMMANDS = {
    'targeted': [
        ('attribution', 900, ['cargo', 'test', '--locked', '-p', 'dracon-system', 'memory_sampling_tests', '--', '--nocapture']),
        ('growth', 300, ['cargo', 'test', '--locked', '-p', 'dracon-system', 'service_growth', '--', '--nocapture']),
        ('persistence', 300, ['cargo', 'test', '--locked', '-p', 'dracon-system', 'tmux_health', '--', '--nocapture']),
    ],
    'workspace': [
        ('workspace-tests', 1800, ['cargo', 'test', '--workspace', '--locked']),
        ('workspace-clippy', 1200, ['cargo', 'clippy', '--workspace', '--locked', '--', '-D', 'warnings']),
        ('workspace-release', 2400, ['cargo', 'build', '--release', '--locked']),
        ('workspace-deny', 600, ['cargo', 'deny', 'check']),
    ],
}


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=COMMANDS)
    phase = parser.parse_args().phase
    state = json.loads((OUT / 'source-state.json').read_text())
    root = Path(state['snapshot'])
    assert root.is_absolute() and root != Path.cwd().parent
    # Refuse drift in every recorded tracked source/config/lock/script input.
    for path, expected in state['source_hashes'].items():
        assert hashlib.sha256((root / path).read_bytes()).hexdigest() == expected, path
    for path, expected in json.loads((OUT / 'sdk-assets.json').read_text()).items():
        assert hashlib.sha256((root / path).read_bytes()).hexdigest() == expected, path
    env = os.environ.copy()
    env.update(state['environment'])
    # Configuration's relative helper path points into this snapshot, never live target.
    env['DRACON_EPOCH_TEST_HELPER'] = str(root / 'target/debug/dracon-sync')
    results = []
    for name, bound, argv in COMMANDS[phase]:
        started, tick = stamp(), time.monotonic()
        print(f'{started} starting {name}: timeout {bound} ' + ' '.join(argv), flush=True)
        with (OUT / f'{name}.log').open('xb') as log:
            log.write((f'cwd={root}\nstarted_at={started}\ncommand=timeout {bound} ' + ' '.join(argv) + '\n').encode())
            log.flush()
            result = subprocess.run(['timeout', str(bound), *argv], cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
        row = dict(name=name, command=argv, timeout_seconds=bound, cwd=str(root),
                   started_at=started, finished_at=stamp(), elapsed_seconds=time.monotonic()-tick,
                   exit=result.returncode, log=f'{name}.log',
                   log_sha256=hashlib.sha256((OUT/f'{name}.log').read_bytes()).hexdigest())
        results.append(row)
        (OUT / f'{phase}-results.json').write_text(json.dumps(dict(environment={**state['environment'], 'DRACON_EPOCH_TEST_HELPER':env['DRACON_EPOCH_TEST_HELPER']}, results=results), indent=2)+'\n')
        print(f"{row['finished_at']} {name} exit={result.returncode}", flush=True)
    raise SystemExit(0 if all(r['exit'] == 0 for r in results) else 1)


if __name__ == '__main__':
    main()
