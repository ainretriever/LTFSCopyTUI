#!/usr/bin/env python3
"""Analyze the final 32-GiB READ stream in the dedicated single-thread strace probe."""
import argparse
import json
from pathlib import Path
import re
import statistics


def summary(values, scale=1):
    ordered = sorted(value * scale for value in values)
    return {"count": len(ordered), "sum": sum(ordered), "p50": statistics.median(ordered), "p99": ordered[int((len(ordered) - 1) * .99)], "max": max(ordered)} if ordered else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--bytes", type=int, default=32 * 1024**3)
    args = parser.parse_args()
    calls = []
    for line in args.trace.read_text().splitlines():
        if "SG_IO" not in line:
            continue
        opcode = re.search(r'cmdp="\\x([0-9a-fA-F]{2})', line)
        duration = re.search(r'<([0-9.]+)>$', line)
        if not opcode or not duration:
            raise RuntimeError("unrecognized SG_IO trace format")
        start = float(line.split()[0])
        length = re.search(r'dxfer_len=(\d+)', line)
        resid = re.search(r'resid=(-?\d+)', line)
        calls.append({"op": int(opcode[1], 16), "start": start, "service": float(duration[1]), "payload": int(length[1]) - max(0, int(resid[1])) if length and resid else 0})
    read_indexes = [i for i, call in enumerate(calls) if call["op"] == 8]
    record_count = args.bytes // (512 * 1024)
    if len(read_indexes) < record_count:
        raise RuntimeError("trace does not contain the complete expected stream")
    first, last = read_indexes[-record_count], read_indexes[-1]
    data = calls[first:last + 1]
    reads = [call for call in data if call["op"] == 8]
    if sum(call["payload"] for call in reads) != args.bytes:
        raise RuntimeError("unexpected payload count in final READ stream")
    wall = data[-1]["start"] + data[-1]["service"] - data[0]["start"]
    gaps = [max(0, after["start"] - (before["start"] + before["service"])) for before, after in zip(data, data[1:])]
    by_op = {f"0x{opcode:02x}": summary([call["service"] for call in data if call["op"] == opcode], 1000) for opcode in sorted({call["op"] for call in data})}
    bins = []
    for begin in range(0, len(reads), 2048):
        group = reads[begin:begin + 2048]
        elapsed = group[-1]["start"] + group[-1]["service"] - group[0]["start"]
        bins.append({"GiB_start": begin / 2048, "MB_s": sum(call["payload"] for call in group) / elapsed / 1e6, "READ_service_ms": statistics.mean(call["service"] for call in group) * 1000})
    result = {"note": "strace-instrumented stdout probe; includes tracer overhead and excludes startup/index inspection", "payload_bytes": args.bytes, "stream_wall_seconds": wall, "stream_MB_s": args.bytes / wall / 1e6, "host_gap_ms": summary(gaps, 1000), "host_gap_percent_of_wall": sum(gaps) / wall * 100, "host_gaps_over_1ms": sum(gap > .001 for gap in gaps), "host_gaps_over_10ms": sum(gap > .01 for gap in gaps), "service_ms_by_opcode": by_op, "GiB_bins": bins}
    args.trace.with_suffix(".analysis.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result), flush=True)


if __name__ == "__main__":
    main()
