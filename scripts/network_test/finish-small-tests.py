#!/usr/bin/env python3
"""Serialize host concurrency probes and tape benchmark after host matrix completes."""
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

BASE = Path('/home/ain/tapecpy-small-20261024')
ROOTS = {'local': BASE / 'local', 'nfs': Path('/mnt/tapecpy_nfs/small-20261024'),
         'smb': Path('/mnt/tapecpy_smb/small-20261024')}


def read_file(path):
    with path.open('rb', buffering=0) as source:
        data = source.read(524288)
        if source.read(1):
            raise RuntimeError('unexpected file size')
    return len(data), hashlib.sha256(data).hexdigest()


def main():
    deadline = time.monotonic() + 3600
    while not (BASE / 'host-completed.txt').exists():
        if time.monotonic() > deadline:
            raise RuntimeError('host matrix did not finish; tape untouched by this script')
        time.sleep(10)
    reference = None
    for repeat in range(3):
        for nested in (False, True):
            for protocol in list(ROOTS)[repeat:] + list(ROOTS)[:repeat]:
                root = ROOTS[protocol] / ('source-4096-' + ('nested' if nested else 'flat'))
                paths = sorted(root.rglob('*.bin'))
                assert len(paths) == 10000
                for workers in ([1, 4, 16] if repeat % 2 == 0 else [16, 4, 1]):
                    started = time.monotonic()
                    if workers == 1:
                        result = list(map(read_file, paths))
                    else:
                        with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
                            result = list(pool.map(read_file, paths))
                    elapsed = time.monotonic() - started
                    if reference is None:
                        reference = result
                    assert result == reference
                    row = dict(repeat=repeat, nested=nested, protocol=protocol, workers=workers,
                               seconds=elapsed, MB_s=40960000/elapsed/1e6, files_s=10000/elapsed,
                               verified=True)
                    with (BASE / 'concurrency-results.jsonl').open('a') as output:
                        output.write(json.dumps(row) + '\n')
    (BASE / 'concurrency-completed.txt').write_text('done\n')
    env = dict(os.environ, TAPECPY_RECORD_TARGET='/record-benchmark-20261024')
    with (BASE / 'record-results.log').open('x') as output:
        subprocess.run([str(BASE / 'record_bench'), 'app::performance_tests::tape_record_sizes',
                        '--exact', '--ignored', '--nocapture'], env=env, stdout=output,
                       stderr=subprocess.STDOUT, check=True, timeout=1800)
    (BASE / 'record-completed.txt').write_text('write committed and all record hashes verified\n')


if __name__ == '__main__':
    main()
