#!/usr/bin/env python3
"""Verify recorded gates/citations/source scope; never execute tests or host controls."""
import datetime
import hashlib
import json
from pathlib import Path
import re

OUT = Path(__file__).resolve().parent
LIVE = OUT.parents[4]
REQUIRED = {
    'workspace-tests': ['cargo','test','--workspace','--locked'],
    'workspace-clippy': ['cargo','clippy','--workspace','--locked','--','-D','warnings'],
    'workspace-release': ['cargo','build','--release','--locked'],
    'workspace-deny': ['cargo','deny','check'],
}
SELECTORS = {'attribution':'memory_sampling_tests', 'growth':'service_growth', 'persistence':'tmux_health'}


def main():
    state = json.loads((OUT/'source-state.json').read_text())
    root = Path(state['snapshot'])
    all_rows = []
    environments = []
    for filename in ['targeted-results.json','workspace-results.json','remaining-results.json']:
        report = json.loads((OUT/filename).read_text())
        environments.append(report['environment'])
        all_rows.extend(report['results'])
    assert len({r['name'] for r in all_rows}) == len(all_rows), 'duplicate gate receipts'
    assert {r['name'] for r in all_rows} == set(REQUIRED) | set(SELECTORS), 'missing gate'
    summaries = {}
    for row in all_rows:
        assert row['exit'] == 0 and row['cwd'] == str(root), row['name']
        assert datetime.datetime.fromisoformat(row['finished_at']) >= datetime.datetime.fromisoformat(row['started_at'])
        assert row['elapsed_seconds'] >= 0
        log = OUT/row['log']; data = log.read_bytes()
        assert hashlib.sha256(data).hexdigest() == row['log_sha256'], log.name
        if row['name'] in REQUIRED:
            assert row['command'] == REQUIRED[row['name']], row['command']
        else:
            assert row['command'] == ['cargo','test','--locked','-p','dracon-system',SELECTORS[row['name']],'--','--nocapture']
        if row['name'] in SELECTORS or row['name'] == 'workspace-tests':
            found = re.findall(r'test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;', data.decode())
            assert found and all(r[0]=='ok' and int(r[2])==0 for r in found), row['name']
            totals = dict(summaries=len(found), passed=sum(int(r[1]) for r in found), failed=sum(int(r[2]) for r in found), ignored=sum(int(r[3]) for r in found))
            assert totals['passed'] > 0, 'empty test selection'
            summaries[row['name']] = totals
    for env in environments:
        assert Path(env['CARGO_TARGET_DIR']) == root/'target'
        assert Path(env['DRACON_EPOCH_TEST_HELPER']) == root/'target/debug/dracon-sync'
    for path, sha in state['source_hashes'].items():
        assert hashlib.sha256((root/path).read_bytes()).hexdigest() == sha, path
    assets = OUT/'sdk-assets.json'
    assert hashlib.sha256(assets.read_bytes()).hexdigest() == state['sdk_assets_manifest_sha256']
    for path, sha in json.loads(assets.read_text()).items():
        f=root/path
        assert f.resolve().is_relative_to(root) and hashlib.sha256(f.read_bytes()).hexdigest()==sha, path
    # Compare current inputs honestly; newer sibling work is not qualified by
    # frozen receipts. Performance implementation drift would invalidate this report.
    drift=[]
    for path, sha in state['source_hashes'].items():
        current = LIVE/path
        if not current.is_file() or hashlib.sha256(current.read_bytes()).hexdigest()!=sha:
            drift.append(path)
    assert not any(p.startswith('dracon-system/') for p in drift), 'performance source changed after qualification'
    assert not any(not p.startswith('dracon-sync/') for p in drift), 'other qualification inputs changed'
    report = OUT.parent/'README.md'
    links = re.findall(r'\[[^\]]+\]\(([^)]+)\)', report.read_text())
    for link in links:
        if not link.startswith(('https://','http://')):
            assert (report.parent/link.split('#')[0]).is_file(), link
    interruption = json.loads((OUT/'interruption.json').read_text())
    assert interruption['interrupted_gate_exit'] is None
    result = dict(verified_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  scope='recorded frozen workspace only; newer sibling source unqualified',
                  summaries=summaries, source_hashes_match_frozen=True,
                  current_unqualified_sibling_paths=drift, local_citations=len(links),
                  report_sha256=hashlib.sha256(report.read_bytes()).hexdigest())
    (OUT/'verification.json').write_text(json.dumps(result,indent=2)+'\n')
    print('PASS: exact seven gate receipts/log hashes; all four workspace gates exit0; nonempty targeted behaviors; frozen source/SDK integrity; citations; isolated target/helper; interrupted exit stays unknown')
    print(json.dumps(result,indent=2))


if __name__ == '__main__':
    main()
