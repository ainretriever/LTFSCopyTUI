#!/usr/bin/env python3
"""Read-only warm-cache small-file baseline; deliberately does not access tape.

Repeated reads share client/server caches. Python/thread overhead is included;
these figures are host baselines, not measurements of tapecpy's Rust pipeline.
"""
import concurrent.futures
import hashlib
import json
from pathlib import Path
import time

ROOTS = {
    'local': Path('/home/ain/tapecpy-perf-20261022/source-32g/small'),
    'nfs': Path('/mnt/tapecpy_nfs/ltfscopytui-20261023/payload/small'),
    'smb': Path('/mnt/tapecpy_smb/ltfscopytui-20261023/payload/small'),
}


def read_file(path):
    with path.open('rb', buffering=0) as source:
        data = source.read(512 * 1024)
        if source.read(1):
            raise RuntimeError(f'not a small file: {path}')
    return len(data), hashlib.sha256(data).hexdigest()


def main():
    reference = None
    for protocol, root in ROOTS.items():
        started = time.monotonic()
        files = sorted(root.rglob('*'))
        files = [path for path in files if path.is_file()]
        if len(files) != 10_000:
            raise RuntimeError(f'{protocol}: expected 10000 files, found {len(files)}')
        print(json.dumps({'protocol': protocol, 'operation': 'enumerate_stat',
                          'files': len(files), 'seconds': time.monotonic() - started}), flush=True)
        for workers in (1, 4, 16, 1):
            started = time.monotonic()
            if workers == 1:
                results = list(map(read_file, files))
            else:
                with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
                    results = list(pool.map(read_file, files))
            elapsed = time.monotonic() - started
            if reference is None:
                reference = results
            if results != reference:
                raise RuntimeError(f'{protocol}: content mismatch')
            total = sum(size for size, _ in results)
            print(json.dumps({'protocol': protocol, 'operation': 'open_read_hash_close',
                              'workers': workers, 'files': len(files), 'bytes': total,
                              'seconds': elapsed, 'files_per_second': len(files) / elapsed,
                              'MB_per_second': total / elapsed / 1e6,
                              'matches_local': True}), flush=True)


if __name__ == '__main__':
    main()
