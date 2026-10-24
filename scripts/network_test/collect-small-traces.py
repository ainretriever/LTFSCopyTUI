#!/usr/bin/env python3
"""Collect syscall counts after benchmarks, then check the retained LTFS volume."""
import json
import os
from pathlib import Path
import subprocess
import time

base = Path('/home/ain/tapecpy-small-20261024')
deadline = time.monotonic() + 3600
while not (base / 'record-completed.txt').exists():
    if time.monotonic() > deadline:
        raise RuntimeError('record test did not complete')
    time.sleep(10)
for protocol, root in [('local', base / 'local'), ('nfs', Path('/mnt/tapecpy_nfs/small-20261024')),
                       ('smb', Path('/mnt/tapecpy_smb/small-20261024'))]:
    for source, filename in [('/proc/self/mountstats', 'mountstats'), ('/proc/fs/cifs/Stats', 'cifs-stats')]:
        try:
            (base / f'{protocol}-{filename}-before.txt').write_text(Path(source).read_text())
        except OSError as error:
            (base / f'{protocol}-{filename}-unavailable.txt').write_text(str(error))
    spec = dict(mode='destination', root=str(root / f'trace-destination-{protocol}'),
                size=4096, count=1000, nested=False)
    env = dict(os.environ, TAPECPY_HOST_BENCH_SPEC=json.dumps(spec))
    with (base / f'trace-{protocol}.log').open('x') as output:
        subprocess.run(['strace', '-f', '-c', '-w', '-e',
                        'trace=openat,openat2,read,write,close,lseek,syncfs',
                        '-o', str(base / f'trace-{protocol}.txt'), str(base / 'host_bench'),
                        'app::performance_tests::host_small_files', '--exact', '--ignored', '--nocapture'],
                       env=env, stdout=output, stderr=output, check=True, timeout=300)
    for source, filename in [('/proc/self/mountstats', 'mountstats'), ('/proc/fs/cifs/Stats', 'cifs-stats')]:
        try:
            (base / f'{protocol}-{filename}-after.txt').write_text(Path(source).read_text())
        except OSError:
            pass
with (base / 'final-diagnose.txt').open('x') as output:
    subprocess.run(['/home/ain/.local/bin/tapecpy', 'diagnose', '/dev/sg0'],
                   stdout=output, stderr=output, check=True, timeout=300)
(base / 'all-completed.txt').write_text('all benchmarks, traces and diagnosis completed\n')
