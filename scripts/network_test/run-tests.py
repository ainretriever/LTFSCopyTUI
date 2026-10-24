#!/usr/bin/env python3
"""Serial NFS/SMB host baselines and full tape round trips; stop on first failure."""
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

BASE = Path('/home/ain/tapecpy-network-test')
RESULTS = BASE / 'results'
TOOLS = Path('/home/ain/tapecpy-perf-20261023/tools/scripts')
BIN = Path('/home/ain/tapecpy-perf-20261023/bin')
SOURCE = Path('/home/ain/tapecpy-perf-20261022/source-32g')
MANIFEST = SOURCE / 'manifest.jsonl'
RELATIVE = Path('ltfscopytui-20261023')
JOBS = Path.home() / '.local/state/tapecpy/jobs'
report = {'started_at': datetime.now(timezone.utc).isoformat(), 'status': 'running', 'host_baselines': [], 'runs': []}
report_owned = False


def save():
    temporary = RESULTS / 'network-suite.tmp'
    temporary.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(RESULTS / 'network-suite.json')


def log(message):
    print(datetime.now(timezone.utc).isoformat(), message, flush=True)


def capture(command):
    try:
        return subprocess.check_output([str(arg) for arg in command], text=True, stderr=subprocess.STDOUT)
    except subprocess.CalledProcessError as error:
        raise RuntimeError(str(error) + '\n' + error.output) from error


def verify(selection, restored):
    return json.loads(capture([sys.executable, TOOLS / 'verify_perf_data.py', MANIFEST, selection, restored]))


def wait_exit(state):
    pid = state.get('runner_pid')
    for _ in range(100):
        if not pid or not Path(f'/proc/{pid}').exists():
            return
        time.sleep(.1)
    raise RuntimeError('Runner still alive; refusing overlapping tape access')


def run_job(name, command):
    began = datetime.now(timezone.utc).isoformat()
    output = capture(command)
    log(name + ': ' + output.strip())
    match = re.search(r'(?:job=|job )([a-zA-Z0-9-]+)', output)
    if not match:
        raise RuntimeError('Cannot identify detached job')
    job_id = match[1]
    subprocess.run([sys.executable, str(TOOLS / 'watch_perf_job.py'), job_id, str(RESULTS)], check=True)
    state = json.loads((JOBS / job_id / 'state.json').read_text())
    wait_exit(state)
    spec = json.loads((JOBS / job_id / 'spec.json').read_text())
    endpoint = spec['source'] if spec['operation'] == 'write' else spec['destination']
    expected_type = 'nfs4' if name.startswith('nfs-') else 'cifs'
    if endpoint['filesystem_type'] != expected_type:
        raise RuntimeError('Job did not freeze the expected network mount identity: ' + str(endpoint))
    if spec['operation'] == 'read':
        p = state['progress']
        if p['buffer_used_bytes'] is not None or p['reader_waiting'] or p['writer_waiting']:
            raise RuntimeError('Completed read retained live pipeline state')
    record = {'name': name, 'job_id': job_id, 'started_at': began,
              'finished_at': datetime.now(timezone.utc).isoformat(), 'endpoint': endpoint,
              'completion': state['completion'], 'state_bytes': (JOBS / job_id / 'state.json').stat().st_size,
              'spec_bytes': (JOBS / job_id / 'spec.json').stat().st_size}
    report['runs'].append(record)
    save()
    subprocess.run([sys.executable, str(TOOLS / 'summarize_perf_job.py'), job_id, str(BASE)], check=True)
    subprocess.run([sys.executable, str(TOOLS / 'analyze_channel_samples.py'), str(RESULTS / (job_id + '.samples.jsonl'))], check=True)
    return record


def main():
    global report_owned
    RESULTS.mkdir(parents=True, exist_ok=True)
    if '--resume-after-mount-fix' in sys.argv[1:]:
        previous = json.loads((RESULTS / 'network-suite.json').read_text())
        if previous['status'] != 'failed' or previous['runs'] or 'expected network mount identity' not in previous.get('error', ''):
            raise RuntimeError('Not the expected post-write mount-identity assertion failure')
        job_id = '000000000000000018e127fc7eda00c1-00002bea'
        state = json.loads((JOBS / job_id / 'state.json').read_text())
        spec = json.loads((JOBS / job_id / 'spec.json').read_text())
        if state['phase'] != 'completed' or not state['completion']['index_committed'] or spec['destination']['path'] != '/nfs-network':
            raise RuntimeError('Previous NFS write is not safely committed')
        wait_exit(state)
        (RESULTS / 'network-suite.mount-identity-stopped.json').write_text(json.dumps(previous, ensure_ascii=False, indent=2) + '\n')
        report.update(previous)
        report['status'] = 'running'
        report.pop('error', None)
        report['runs'].append({'name': 'nfs-write', 'job_id': job_id,
            'started_at': spec['created_at'], 'finished_at': state['updated_at'],
            'endpoint': spec['source'], 'completion': state['completion'],
            'state_bytes': (JOBS / job_id / 'state.json').stat().st_size,
            'spec_bytes': (JOBS / job_id / 'spec.json').stat().st_size,
            'mount_identity_issue': 'Completed before autofs identity fix; actual NFS backing independently verified'})
        report['nfs_write_already_completed'] = job_id
        report_owned = True
        save()
        for script, arguments in [('summarize_perf_job.py', [job_id, BASE]),
                                  ('analyze_channel_samples.py', [RESULTS / (job_id + '.samples.jsonl')])]:
            subprocess.run([sys.executable, str(TOOLS / script), *map(str, arguments)], check=True)
        log('RESUMING_FROM_COMMITTED_NFS_WRITE_AFTER_MOUNT_IDENTITY_FIX; no duplicate write')
        tape_round_trips()
        return
    if '--resume-after-capacity-check' in sys.argv[1:]:
        previous = json.loads((RESULTS / 'network-suite.json').read_text())
        capacities = [json.loads(line) for line in (RESULTS / 'partition-capacity.jsonl').read_text().splitlines()]
        if previous['status'] != 'failed' or previous['runs'] or len(previous['host_baselines']) != 4:
            raise RuntimeError('Resume is limited to the diagnosed preflight-only failure')
        if 'capacity warning' not in (RESULTS / 'preflight-error.txt').read_text():
            raise RuntimeError('Expected capacity warning evidence is missing')
        data = next(record for record in capacities if record['role'] == 'data')
        if data['remaining_MiB'] * 1024**2 < previous['payload_copy']['bytes'] * 2:
            raise RuntimeError('Data partition does not have the required verified headroom')
        (RESULTS / 'network-suite.preflight-stopped.json').write_text(json.dumps(previous, ensure_ascii=False, indent=2) + '\n')
        report.update(previous)
        report['status'] = 'running'
        report.pop('error', None)
        report['capacity_warning_acknowledged_for_test'] = True
        report['partition_capacity_evidence'] = capacities
        report_owned = True
        save()
        log('RESUMING_AFTER_VERIFIED_PARTITION_CAPACITY; no data jobs existed before this resume')
        tape_round_trips()
        return
    if (RESULTS / 'network-suite.json').exists():
        raise RuntimeError('Existing suite results; refusing automatic overwrite or tape retry')
    report_owned = True
    save()
    for protocol in ('nfs', 'smb'):
        mount = Path('/mnt/tapecpy_' + protocol)
        if not mount.is_dir():
            raise RuntimeError('Mount unavailable: ' + str(mount))
        report[protocol + '_mount'] = json.loads(capture(['findmnt', '-J', '-T', mount, '-o', 'TARGET,SOURCE,FSTYPE,OPTIONS']))
    entries = [json.loads(line) for line in MANIFEST.read_text().splitlines()]
    payload = Path('/mnt/tapecpy_nfs') / RELATIVE / 'payload'
    payload.mkdir(parents=True, exist_ok=False)
    log('PREPARING_PAYLOAD ' + str(payload))
    began = time.monotonic()
    total = 0
    last_print = began
    for entry in entries:
        destination = payload / entry['path']
        destination.parent.mkdir(parents=True, exist_ok=True)
        digest = hashlib.sha256()
        with (SOURCE / entry['path']).open('rb') as reader, destination.open('xb') as writer:
            while block := reader.read(8 * 1024**2):
                writer.write(block)
                digest.update(block)
                total += len(block)
                if time.monotonic() - last_print > 20:
                    log(f'COPY bytes={total} elapsed={time.monotonic() - began:.1f}s')
                    last_print = time.monotonic()
            writer.flush()
            os.fsync(writer.fileno())
        if digest.hexdigest() != entry['sha256']:
            raise RuntimeError('Local source changed: ' + entry['path'])
    report['payload_copy'] = {'files': len(entries), 'bytes': total, 'seconds': time.monotonic() - began,
                              'note': 'NFS copy with per-file fsync; local source SHA-256 checked during copy'}
    save()
    expected_hash = next(entry['sha256'] for entry in entries if entry['path'] == 'large/stream.bin')
    for protocol in ('nfs', 'smb'):
        root = Path('/mnt/tapecpy_' + protocol) / RELATIVE
        probe = root / ('host-write-' + protocol + '.bin')
        if probe.exists():
            raise RuntimeError('Host baseline path exists')
        began = time.monotonic()
        log('HOST_WRITE ' + protocol)
        subprocess.run(['dd', 'if=' + str(SOURCE / 'large/stream.bin'), 'of=' + str(probe),
                        'bs=4M', 'count=1024', 'conv=fdatasync', 'status=none'], check=True)
        seconds = time.monotonic() - began
        report['host_baselines'].append({'protocol': protocol, 'operation': 'write-fdatasync', 'bytes': 4 * 1024**3,
                                         'seconds': seconds, 'MB_s': 4 * 1024**3 / seconds / 1e6})
        probe.unlink()
        log('HOST_READ_DIRECT_SHA256 ' + protocol)
        began = time.monotonic()
        reader = subprocess.Popen(['dd', 'if=' + str(root / 'payload/large/stream.bin'), 'bs=4M',
                                   'iflag=direct', 'status=none'], stdout=subprocess.PIPE)
        try:
            hashed = subprocess.run(['sha256sum'], stdin=reader.stdout, stdout=subprocess.PIPE, text=True, check=True)
            reader.stdout.close()
            if reader.wait() != 0:
                raise RuntimeError('Direct network read failed; no silent buffered fallback')
        finally:
            if reader.poll() is None:
                reader.terminate()
                reader.wait()
        seconds = time.monotonic() - began
        if hashed.stdout.split()[0] != expected_hash:
            raise RuntimeError('Network payload SHA-256 mismatch')
        report['host_baselines'].append({'protocol': protocol, 'operation': 'read-client-direct-with-sha256',
                                         'bytes': 32 * 1024**3, 'seconds': seconds,
                                         'MB_s': 32 * 1024**3 / seconds / 1e6,
                                         'note': 'Client page-cache bypass only; server cache is not disabled. Hash CPU included.'})
        report[protocol + '_payload_verification'] = [verify(selection, root / 'payload' / selection) for selection in ('mixed', 'small')]
        save()
        log('HOST_BASELINES ' + json.dumps(report['host_baselines'][-2:]))
    if min(record['MB_s'] for record in report['host_baselines']) < 160:
        raise RuntimeError('Host baseline below 160 MB/s; investigate before testing tape streaming')

    tape_round_trips()


def tape_round_trips():
    for protocol in ('nfs', 'smb'):
        root = Path('/mnt/tapecpy_' + protocol) / RELATIVE
        tape_path = '/' + protocol + '-network'
        # The complete independent restore below replaces a duplicate built-in read-back pass.
        command = [BIN / 'tapecpy', 'job', 'start-write', root / 'payload', tape_path, '/dev/sg0']
        if report.get('capacity_warning_acknowledged_for_test'):
            command.append('--ack-capacity-warning')
        if not (protocol == 'nfs' and report.get('nfs_write_already_completed')):
            run_job(protocol + '-write', command)
        restored = root / ('restored-' + protocol)
        record = run_job(protocol + '-read', [BIN / 'perf_read_directory', '/dev/sg0', tape_path, restored])
        selection_root = restored / tape_path.strip('/')
        record['independent_verification'] = [verify(selection, selection_root / selection)
                                               for selection in ('large/stream.bin', 'mixed', 'small')]
        if {path.name for path in selection_root.iterdir()} != {'large', 'mixed', 'small'}:
            raise RuntimeError('Unexpected restored top-level entries')
        record['round_trip_verified'] = True
        save()
        log(protocol.upper() + '_ROUND_TRIP_VERIFIED')
    for operation in ('diagnose', 'health', 'media'):
        output = capture([BIN / 'tapecpy', operation, '/dev/sg0'])
        (RESULTS / ('final-' + operation + '.txt')).write_text(output)
        log(output.strip())
    report['status'] = 'completed'
    report['finished_at'] = datetime.now(timezone.utc).isoformat()
    save()
    log('NETWORK_SUITE_COMPLETED')


try:
    main()
except Exception as error:
    report['status'] = 'failed'
    report['error'] = str(error)
    if report_owned:
        save()
    log('STOPPED: ' + str(error))
    raise
