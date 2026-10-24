#!/usr/bin/env python3
"""Summarize copied benchmark artifacts; does not access devices."""
import collections
import json
from pathlib import Path
import statistics
import sys

root = Path(sys.argv[1])
report = {'host': [], 'concurrency': [], 'records': []}
groups = collections.defaultdict(list)
for line in (root / 'host-results.jsonl').read_text().splitlines():
    row = json.loads(line)
    if row['spec']['mode'] != 'generate':
        spec = row['spec']
        groups[(spec['mode'], row['tag'].split('-')[1], spec['size'], spec['nested'])].append(row['result'])
for (mode, protocol, size, nested), samples in sorted(groups.items()):
    values = {key: statistics.median(sample[key] for sample in samples)
              for key in samples[0] if key != 'verified'}
    if mode == 'destination':
        count = {4096: 10000, 65536: 2048, 1048576: 256}[size]
        values['total_MB_s'] = statistics.median(
            count * size / (sample['prepare_seconds'] + sample['stream_seconds'] + sample['sync_seconds']) / 1e6
            for sample in samples)
    report['host'].append(dict(mode=mode, protocol=protocol, size=size, nested=nested,
                               repeats=len(samples), medians=values))
groups.clear()
for line in (root / 'concurrency-results.jsonl').read_text().splitlines():
    row = json.loads(line)
    groups[(row['protocol'], row['nested'], row['workers'])].append(row['MB_s'])
for (protocol, nested, workers), values in sorted(groups.items()):
    report['concurrency'].append(dict(protocol=protocol, nested=nested, workers=workers,
                                     repeats=len(values), median_MB_s=statistics.median(values)))
record = {}
for line in (root / 'record-results.log').read_text().splitlines():
    for prefix in ('RECORD_WRITE=', 'RECORD_READ=', 'RECORD_SCSI='):
        if line.startswith(prefix):
            record[prefix[:-1]] = json.loads(line[len(prefix):])
groups.clear()
for opcode, size, ns in record['RECORD_SCSI']['opcode_size_ns']:
    groups[(opcode, size)].append(ns)
for (opcode, size), values in sorted(groups.items()):
    ordered = sorted(values)
    report['records'].append(dict(operation='read' if opcode == 8 else 'write', record_bytes=size,
        commands=len(values), bytes=size*len(values), ioctl_seconds=sum(values)/1e9,
        median_us=statistics.median(values)/1000, mean_us=statistics.mean(values)/1000,
        p99_us=ordered[min(len(ordered)-1, int(len(ordered)*0.99))]/1000,
        max_ms=max(values)/1e6,
        payload_MB_per_s_of_ioctl_time=size*len(values)/(sum(values)/1e9)/1e6))
report['record_api'] = []
for operation, key in [('write', 'RECORD_WRITE'), ('read', 'RECORD_READ')]:
    api = collections.defaultdict(list)
    for item in record[key]['timings_size_ns']:
        api[item[0]].append(item[1])
    for size, values in sorted(api.items()):
        report['record_api'].append(dict(operation=operation, record_bytes=size,
            commands=len(values), total_seconds=sum(values)/1e9,
            median_us=statistics.median(values)/1000,mean_us=statistics.mean(values)/1000))
report['record_write'] = {k: v for k, v in record['RECORD_WRITE'].items() if k != 'timings_size_ns'}
report['record_read'] = {k: v for k, v in record['RECORD_READ'].items() if k != 'timings_size_ns'}
report['notes'] = ['Host source timing includes production SourcePipeline + SHA, excludes scan and independent verification.',
                   'Host destination times production safe prepare/open/write/finish/sync methods, excludes simulated input generation and post-sync verification.',
                   'Shared cache not globally dropped: repeated host tests measure warm/revalidated caches, not server-cold disk.',
                   'Record ioctl time excludes application gaps, hash, source generation and buffer initialization; it is not sustained streaming throughput.',
                   'Record bytes are appended as valid LTFS files, existing data preserved; tape test uses one transaction, 3 groups per record size, 64MiB per group.']
print(json.dumps(report, indent=2))
