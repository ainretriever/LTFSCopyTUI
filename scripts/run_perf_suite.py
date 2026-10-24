#!/usr/bin/env python3
"""Run sequential hardware checks after a known initial write; abort on the first failure."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", type=Path)
    parser.add_argument("initial_write_job")
    parser.add_argument("--drive", default="/dev/sg0")
    args = parser.parse_args()
    base = args.base
    binary = base / "bin/tapecpy"
    helper = base / "bin/perf_read_directory"
    tools = base / "tools/scripts"
    results = base / "results"
    jobs = Path.home() / ".local/state/tapecpy/jobs"
    manifest = base / "source-32g/manifest.jsonl"
    runs = []

    def state(job_id):
        return json.loads((jobs / job_id / "state.json").read_text())

    def wait_exit(snapshot):
        pid = snapshot.get("runner_pid")
        if not pid:
            return
        for _ in range(100):
            if not Path(f"/proc/{pid}").exists():
                return
            time.sleep(0.1)
        raise RuntimeError("previous runner still alive; do not overlap tape access")

    deadline = time.monotonic() + 3600
    while True:
        snapshot = state(args.initial_write_job)
        if snapshot["phase"] in ("completed", "failed", "cancelled", "interrupted"):
            if snapshot["phase"] != "completed":
                raise RuntimeError("initial write failed: " + str(snapshot.get("error")))
            wait_exit(snapshot)
            break
        if time.monotonic() > deadline:
            raise RuntimeError("initial job wait timed out; job was NOT cancelled")
        time.sleep(2)

    def run(name, command, selection=None, restored=None):
        began = datetime.now(timezone.utc).isoformat()
        output = subprocess.check_output([str(argument) for argument in command], text=True, stderr=subprocess.STDOUT)
        print(name + ": " + output.strip(), flush=True)
        match = re.search(r"(?:job=|job )([a-zA-Z0-9-]+)", output)
        if not match:
            raise RuntimeError("cannot identify job")
        job_id = match[1]
        # CLI status must remain available even for a large frozen directory plan.
        status = subprocess.check_output([str(binary), "job", "status", job_id], text=True)
        (results / (name + ".initial-status.txt")).write_text(status)
        subprocess.run([sys.executable, str(tools / "watch_perf_job.py"), job_id, str(results)], check=True)
        snapshot = state(job_id)
        wait_exit(snapshot)
        record = {"name": name, "job_id": job_id, "started_at": began, "finished_at": datetime.now(timezone.utc).isoformat(), "state_bytes": (jobs / job_id / "state.json").stat().st_size, "spec_bytes": (jobs / job_id / "spec.json").stat().st_size, "completion": snapshot["completion"]}
        if selection is not None:
            verification = subprocess.check_output([sys.executable, str(tools / "verify_perf_data.py"), str(manifest), selection, str(restored)], text=True)
            record["independent_verification"] = json.loads(verification)
            print(name + " verification: " + verification.strip(), flush=True)
        runs.append(record)
        (results / "suite.json").write_text(json.dumps(runs, ensure_ascii=False, indent=2) + "\n")

    run("large-read", [binary, "job", "start-read", "/large-baseline.bin", base / "restored-large.bin", args.drive], "large/stream.bin", base / "restored-large.bin")
    run("mixed-write", [binary, "job", "start-write", base / "source-32g/mixed", "/mixed-baseline", args.drive, "--read-back-verify"])
    run("mixed-read", [helper, args.drive, "/mixed-baseline", base / "restored-mixed"], "mixed", base / "restored-mixed/mixed-baseline")
    run("small-write", [binary, "job", "start-write", base / "source-32g/small", "/small-baseline", args.drive, "--read-back-verify"])
    run("small-read", [helper, args.drive, "/small-baseline", base / "restored-small"], "small", base / "restored-small/small-baseline")
    for operation in ("diagnose", "health"):
        output = subprocess.check_output([str(binary), operation, args.drive], text=True, stderr=subprocess.STDOUT)
        (results / ("final-" + operation + ".txt")).write_text(output)
        print(output, flush=True)
    print("SUITE_COMPLETED", flush=True)


if __name__ == "__main__":
    main()
