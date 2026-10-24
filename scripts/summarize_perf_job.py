#!/usr/bin/env python3
"""Summarize measured job history without treating startup/finalization as streaming."""
import argparse
from datetime import datetime
import json
from pathlib import Path
import statistics


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("job_id")
    parser.add_argument("base", type=Path)
    args = parser.parse_args()
    results = args.base / "results"
    state = json.loads((results / (args.job_id + ".final.json")).read_text())
    samples = [json.loads(line) for line in (results / (args.job_id + ".samples.jsonl")).read_text().splitlines()]
    history = state["progress"]["throughput_history"]
    rates = [sample["bytes_per_second"] / 1e6 for sample in history]
    # Report every history sample as well as a window excluding only ramp/tail samples.
    trimmed = rates[10:-10] if len(rates) > 30 else rates
    def stats(values):
        if not values:
            return None
        ordered = sorted(values)
        return {"samples": len(values), "min_MB_s": round(min(values), 3), "p10_MB_s": round(ordered[int((len(values) - 1) * 0.1)], 3), "median_MB_s": round(statistics.median(values), 3), "p90_MB_s": round(ordered[int((len(values) - 1) * 0.9)], 3), "max_MB_s": round(max(values), 3), "mean_MB_s": round(statistics.mean(values), 3)}
    observed_phases = {}
    for sample in samples:
        observed_phases.setdefault(sample["message"], {"first_observed": sample["observed_at"], "first_bytes": sample["progress"]["bytes_completed"]})["last_observed"] = sample["observed_at"]
    rss = [int(sample["memory"]["VmHWM"].split()[0]) for sample in samples if "VmHWM" in sample.get("memory", {})]
    result = {"job_id": args.job_id, "operation": state["spec"]["operation"], "phase": state["phase"], "bytes": state["progress"]["bytes_completed"], "items": state["progress"]["items_completed"], "total_wall_seconds": round((datetime.fromisoformat(state["updated_at"]) - datetime.fromisoformat(state["spec"]["created_at"])).total_seconds(), 3), "all_history": stats(rates), "history_without_first_last_10": stats(trimmed), "peak_RSS_KiB_observed": max(rss) if rss else None, "max_state_bytes_observed": max(sample["state_bytes"] for sample in samples), "completion": state["completion"], "error": state.get("error"), "observed_phases": observed_phases}
    (results / (args.job_id + ".summary.json")).write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(result, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
