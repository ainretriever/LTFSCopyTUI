#!/usr/bin/env python3
"""Read-only tape audit via tapecpy stdout, independent SHA1, no payload files.
Requires a fresh inventory made with perf_audit_inventory and a new output dir.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def sha1_expected(entry):
    value = entry['hashes']['ltfs.hash.sha1sum'].strip().lower()
    if len(value) != 40 or any(c not in '0123456789abcdef' for c in value):
        raise ValueError('Invalid SHA1 for ' + entry['path'])
    if entry.get('symlink') is not None:
        raise ValueError('Symlink audit requires separate semantics')
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inventory', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    entries = json.loads((args.inventory / 'files.json').read_text())
    inventory = json.loads((args.inventory / 'inventory.json').read_text())
    for entry in entries:
        sha1_expected(entry)
    args.output.mkdir()  # Never overwrite/resume past evidence silently.
    lock = (args.output.parent / 'audit.lock').open('a')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    binary = Path.home() / '.local/bin/tapecpy'
    started = time.time()
    state = dict(status='running', volume=inventory, started_unix=started,
                 files_total=len(entries), files_checked=0, passed=0, failed=0,
                 bytes_checked=0, current_file=None, current_bytes=0,
                 current_sha1=None, message='Starting read-only audit')
    (args.output / 'binary.sha256').write_text(subprocess.check_output(['sha256sum', str(binary)], text=True))
    def save():
        state['updated_unix'] = time.time()
        temp = args.output / 'progress.tmp'
        temp.write_text(json.dumps(state, ensure_ascii=False, indent=2)+'\n')
        os.replace(temp, args.output / 'progress.json')
    child = None
    stopped = False
    def stop(signum, frame):
        nonlocal stopped
        stopped = True
        if child is not None and child.poll() is None:
            child.terminate()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    save()
    try:
        with (args.output / 'files.jsonl').open('x') as results:
            for number, entry in enumerate(entries):
                if stopped:
                    break
                expected = sha1_expected(entry)
                state.update(current_file=entry['path'], current_bytes=0, current_sha1=None,
                             message='Reading and hashing file')
                save()
                file_start = time.monotonic()
                digest = hashlib.sha1()
                count = 0
                last_save = 0
                with (args.output / f'{number:03}-read.stderr').open('x') as errors:
                    child = subprocess.Popen([str(binary), 'read', '/dev/sg0', entry['path']],
                                             stdout=subprocess.PIPE, stderr=errors)
                    try:
                        while data := child.stdout.read(4 * 1024**2):
                            count += len(data)
                            digest.update(data)
                            now = time.monotonic()
                            if now-last_save >= 5:
                                state.update(current_bytes=count)
                                save()
                                last_save = now
                        code = child.wait()
                    finally:
                        child.stdout.close()
                        if child.poll() is None:
                            child.terminate()
                            child.wait()
                        child = None
                actual = digest.hexdigest()
                verified = code == 0 and count == entry['length'] and actual == expected
                record = dict(path=entry['path'], bytes=count, expected_bytes=entry['length'],
                              expected_sha1=expected, actual_sha1=actual, verified=verified,
                              read_exit_code=code, seconds=time.monotonic()-file_start)
                results.write(json.dumps(record, ensure_ascii=False)+'\n')
                results.flush()
                os.fsync(results.fileno())
                if not stopped:
                    state['files_checked'] += 1
                    state['bytes_checked'] += count
                    state['passed' if verified else 'failed'] += 1
                state.update(current_bytes=count, current_sha1=actual)
                save()
                print(json.dumps(record, ensure_ascii=False), flush=True)
                if code != 0:
                    state['message'] = 'Read command failed; stopping, not treating partial hash as full result'
                    break
        if stopped:
            state['status'] = 'interrupted'
        elif state['files_checked'] != len(entries):
            state['status'] = 'read_failed'
        else:
            state['status'] = 'completed' if state['failed'] == 0 else 'completed_with_mismatches'
        save()
    except BaseException as error:
        state.update(status='error', message=str(error))
        save()
        raise


if __name__ == '__main__':
    main()
