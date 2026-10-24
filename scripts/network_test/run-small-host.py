#!/usr/bin/env python3
"""Run explicit Rust host benchmarks; preserve all data/results, no tape access."""
import json
import os
from pathlib import Path
import subprocess

BASE = Path('/home/ain/tapecpy-small-20261024')
BINARY = BASE / 'host_bench'
ROOTS = {'local': BASE / 'local', 'nfs': Path('/mnt/tapecpy_nfs/small-20261024'),
         'smb': Path('/mnt/tapecpy_smb/small-20261024')}
CASES = [(4096, 10000), (65536, 2048), (1048576, 256)]


def call(spec, tag):
    report = BASE / 'host-results.jsonl'
    if report.exists() and any(json.loads(line)['tag'] == tag for line in report.read_text().splitlines()):
        return
    env = dict(os.environ, TAPECPY_HOST_BENCH_SPEC=json.dumps(spec))
    completed = subprocess.run([str(BINARY), 'app::performance_tests::host_small_files',
                                '--exact', '--ignored', '--nocapture'], env=env,
                               capture_output=True, text=True, timeout=300)
    (BASE / (tag + '.log')).write_text(completed.stdout + completed.stderr)
    if completed.returncode:
        raise RuntimeError(f'{tag}: failed, see log')
    row = next(json.loads(line.split('=', 1)[1]) for line in completed.stdout.splitlines()
               if line.startswith('BENCH_RESULT='))
    with (BASE / 'host-results.jsonl').open('a') as output:
        output.write(json.dumps(dict(tag=tag, **row)) + '\n')
    print(tag, flush=True)


def main():
    for root in ROOTS.values():
        root.mkdir(exist_ok=True)
    for size, count in CASES:
        for nested in (False, True):
            case = f'{size}-' + ('nested' if nested else 'flat')
            for protocol in ('local', 'nfs'):
                call(dict(mode='generate', root=str(ROOTS[protocol] / ('source-' + case)),
                          size=size, count=count, nested=nested), f'generate-{protocol}-{case}')
    for repeat in range(3):
        protocols = list(ROOTS)
        protocols = protocols[repeat:] + protocols[:repeat]
        cases = CASES if repeat % 2 == 0 else list(reversed(CASES))
        for size, count in cases:
            for nested in (False, True):
                case = f'{size}-' + ('nested' if nested else 'flat')
                for protocol in protocols:
                    for mode in ('source', 'destination'):
                        path = ROOTS[protocol] / (('source-' + case) if mode == 'source'
                                                else f'destination-{protocol}-r{repeat}-{case}')
                        call(dict(mode=mode, root=str(path), size=size, count=count,
                                  nested=nested), f'{mode}-{protocol}-r{repeat}-{case}')
    (BASE / 'host-completed.txt').write_text('all host benchmarks completed\n')


if __name__ == '__main__':
    main()
