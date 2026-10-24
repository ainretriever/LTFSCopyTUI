#!/usr/bin/env python3
"""Paired, read-only host A/B, then normal LTFS SMB write+readback A/B."""
import json
import os
from pathlib import Path
import subprocess

BASE = Path('/home/ain/tapecpy-source-opt-20261024')
ROOTS = {'local': Path('/home/ain/tapecpy-small-20261024/local'),
         'nfs': Path('/mnt/tapecpy_nfs/small-20261024'),
         'smb': Path('/mnt/tapecpy_smb/small-20261024')}
CASES = [(4096, 10000), (65536, 2048), (1048576, 256)]


def run(version, test, tag, prefix, env=None):
    result = subprocess.run([str(BASE/version), 'app::performance_tests::'+test,
                             '--exact', '--ignored', '--nocapture'],
                            env=dict(os.environ, **(env or {})), capture_output=True,
                            text=True, timeout=900)
    (BASE/(tag+'.log')).write_text(result.stdout+result.stderr)
    if result.returncode:
        raise RuntimeError(f'{tag}: see retained log')
    row = next(json.loads(line.split('=', 1)[1]) for line in result.stdout.splitlines()
               if line.startswith(prefix+'='))
    with (BASE/'results.jsonl').open('a') as output:
        output.write(json.dumps(dict(tag=tag, version=version, result=row))+'\n')
    print(tag, flush=True)


def main():
    if (BASE/'results.jsonl').exists():
        raise RuntimeError('refuse duplicate run')
    for repeat in range(3):
        versions = ['before', 'after'] if repeat % 2 == 0 else ['after', 'before']
        for version in versions:
            run(version, 'source_prefill_comparison', f'prefill-r{repeat}-{version}', 'PREFILL_RESULT')
        protocols = list(ROOTS)[repeat:]+list(ROOTS)[:repeat]
        for size, count in (CASES if repeat % 2 == 0 else list(reversed(CASES))):
            for nested in [False, True]:
                case = f'{size}-'+('nested' if nested else 'flat')
                for protocol in protocols:
                    spec = dict(mode='source', root=str(ROOTS[protocol]/('source-'+case)),
                                size=size, count=count, nested=nested)
                    for version in versions:
                        run(version, 'host_small_files', f'host-{protocol}-{case}-r{repeat}-{version}',
                            'BENCH_RESULT', dict(TAPECPY_HOST_BENCH_SPEC=json.dumps(spec)))
    (BASE/'host-completed.txt').write_text('host A/B completed\n')
    for version in ['before', 'after']:
        spec = dict(source=str(ROOTS['smb']/'source-4096-flat'),
                    target=f'/source-optimization-{version}-20261024')
        run(version, 'source_write_comparison', f'tape-smb-{version}', 'WRITE_COMPARE_RESULT',
            dict(TAPECPY_WRITE_COMPARE_SPEC=json.dumps(spec)))
    (BASE/'all-completed.txt').write_text('all source A/B completed\n')


if __name__ == '__main__':
    main()
