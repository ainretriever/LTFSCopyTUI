#!/usr/bin/env python3
"""Describe observed pipeline states; snapshots do not prove absence of subsecond stalls."""
import json
from pathlib import Path
import statistics
import sys

path = Path(sys.argv[1])
samples = []
seen = set()
for line in path.read_text().splitlines():
    row = json.loads(line)
    progress = row['progress']
    stamp = progress.get('performance_updated_at')
    if row['message'] not in ('WritingData', 'Read operation 正在运行') or not stamp or stamp in seen:
        continue
    seen.add(stamp)
    samples.append({'observed_at': row['observed_at'], **progress})
report = {'note': 'Distinct persisted performance snapshots. Bulk excludes first/last GiB of large file; tail includes mixed/small files. Waiting flags are observations, not proof of no subsecond stalls.', 'groups': {}}
for name, selected in (
    ('all_data', samples),
    ('large_file_bulk', [p for p in samples if 1024**3 <= p['bytes_completed'] < 31 * 1024**3]),
    ('mixed_small_tail', [p for p in samples if p['bytes_completed'] >= 32 * 1024**3]),
):
    rates = [p['tape_bytes_per_second'] / 1e6 for p in selected if p.get('tape_bytes_per_second') is not None]
    report['groups'][name] = {
        'snapshots': len(selected), 'median_MB_s': statistics.median(rates) if rates else None,
        'min_MB_s': min(rates) if rates else None,
        'reader_waiting_snapshots': sum(p['reader_waiting'] for p in selected),
        'writer_waiting_snapshots': sum(p['writer_waiting'] for p in selected),
        'buffer_at_least_90pct_snapshots': sum(p.get('buffer_capacity_bytes') is not None and p['buffer_capacity_bytes'] > 0 and (p.get('buffer_used_bytes') or 0) * 10 >= p['buffer_capacity_bytes'] * 9 for p in selected),
        'slow_snapshots': [{k: p.get(k) for k in ('observed_at', 'bytes_completed', 'tape_bytes_per_second', 'buffer_used_bytes', 'buffer_capacity_bytes', 'reader_waiting', 'writer_waiting')} for p in selected if p.get('tape_bytes_per_second') is not None and p['tape_bytes_per_second'] < 100_000_000],
    }
output = path.with_suffix('.pipeline.json')
output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
print(json.dumps({'output': str(output), 'groups': {name: {k: v for k, v in group.items() if k != 'slow_snapshots'} for name, group in report['groups'].items()}}))
