#!/usr/bin/env python3
"""Summarize deduplicated channel telemetry, excluding inactive-channel placeholders."""
import argparse
import json
from pathlib import Path
import statistics


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("samples", type=Path)
    args = parser.parse_args()
    seen = set()
    samples = []
    for line in args.samples.read_text().splitlines():
        row = json.loads(line)
        progress = row["progress"]
        timestamp = progress.get("telemetry_updated_at")
        if not timestamp or timestamp in seen:
            continue
        seen.add(timestamp)
        if row["message"] not in ("WritingData", "Read operation 正在运行"):
            continue
        window = progress.get("channel_counter_window")
        samples.append({"timestamp": timestamp, "observed_bytes": progress["bytes_completed"],
                        "observed_MB_s": (progress.get("tape_bytes_per_second") or 0) / 1e6,
                        "rates": progress["channel_rates"], "counter_window": window})
    regions = []
    for lower, upper in ((0, 5), (5, 14), (14, 17), (17, 30), (30, 32.01)):
        selected = [sample for sample in samples if lower <= sample["observed_bytes"] / 1024**3 < upper]
        channels = []
        for channel in range(16):
            rates = [rate for sample in selected for rate in sample["rates"] if rate["channel"] == channel]
            values = [rate["log10_bit_error_rate"] for rate in rates
                      if rate["ccp_advanced"] and rate["log10_bit_error_rate"] is not None]
            deltas = []
            for sample in selected:
                window = sample["counter_window"]
                if not window or min(len(window["before"]), len(window["after"])) <= channel:
                    continue
                before, after = window["before"][channel], window["after"][channel]
                deltas.append({key: after[key] - before[key] if after[key] >= before[key] else None for key in before})
            channels.append({"channel": channel, "median_log10_rate": statistics.median(values) if values else None,
                             "worst_valid_log10_rate": max(values) if values else None,
                             "no_ccp_advance_samples": sum(not rate["ccp_advanced"] for rate in rates),
                             "counter_deltas": deltas})
        regions.append({"GiB_range": [lower, upper], "samples": len(selected),
                        "median_observed_MB_s": statistics.median([sample["observed_MB_s"] for sample in selected]) if selected else None,
                        "channels": channels})
    result = {"note": "0-based channels; progress byte position is approximate. Null rate may mean zero C1 errors; inactive -2.98 placeholders excluded. Raw window has exact sample block, but no window start block. Counter reset yields null delta.",
              "telemetry_samples": len(samples), "raw_counter_samples": sum(sample["counter_window"] is not None for sample in samples),
              "regions": regions, "samples": samples}
    output = args.samples.with_suffix(".channels.json")
    output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({"output": str(output), "samples": len(samples),
                      "regions": [{key: value for key, value in region.items() if key != "channels"} for region in regions]}, ensure_ascii=False))


if __name__ == "__main__":
    main()
