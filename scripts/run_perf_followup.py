#!/usr/bin/env python3
"""Follow successful suite with known nonzero slices and a separate, traced read probe."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time

base = Path(sys.argv[1])
results = base / "results"
tools = base / "tools/scripts"
deadline = time.monotonic() + 3600
while not (results / "final-health.txt").exists():
    if time.monotonic() > deadline:
        raise RuntimeError("suite did not finish; no follow-up tape commands were sent")
    time.sleep(2)
time.sleep(1)
output = subprocess.check_output([str(base / "bin/perf_read_slices"), "/dev/sg0", "/large-baseline.bin", str(base / "restored-slices")], text=True)
print(output, flush=True)
job_id = re.search(r"job=([a-zA-Z0-9-]+)", output)[1]
subprocess.run([sys.executable, str(tools / "watch_perf_job.py"), job_id, str(results)], check=True)
state = json.loads((Path.home() / ".local/state/tapecpy/jobs" / job_id / "state.json").read_text())
pid = state.get("runner_pid")
for _ in range(100):
    if not pid or not Path(f"/proc/{pid}").exists():
        break
    time.sleep(.1)
else:
    raise RuntimeError("slice runner has not exited")
verified = []
with (base / "source-32g/large/stream.bin").open("rb") as source:
    for number, (offset, length) in enumerate([(17, 123), (140, 600000), (600140, 1048700)]):
        source.seek(offset)
        expected = source.read(length)
        restored = (base / f"restored-slices/slice-{number}.bin").read_bytes()
        if len(expected) != length or restored != expected:
            raise RuntimeError("nonzero byteoffset payload mismatch")
        verified.append({"file": f"slice-{number}.bin", "source_offset": offset, "length": length, "sha256": hashlib.sha256(restored).hexdigest()})
(results / "nonzero-slices.json").write_text(json.dumps({"job_id": job_id, "verified": verified}, indent=2) + "\n")
print("NONZERO_SLICES_VERIFIED", flush=True)
if "--skip-trace" in sys.argv[2:]:
    print("FOLLOWUP_COMPLETED_WITHOUT_TRACE", flush=True)
    sys.exit(0)
started = datetime.now(timezone.utc)
with (results / "large-read-ioctl.stderr.txt").open("w") as errors:
    subprocess.run(["strace", "-ttt", "-T", "-xx", "-s", "16", "-e", "trace=ioctl", "-o", str(results / "large-read-ioctl.trace"), str(base / "bin/tapecpy"), "read", "/dev/sg0", "/large-baseline.bin"], stdout=subprocess.DEVNULL, stderr=errors, check=True)
(results / "traced-read.json").write_text(json.dumps({"started_at": started.isoformat(), "finished_at": datetime.now(timezone.utc).isoformat(), "note": "Instrumented stdout-to-/dev/null probe; not an untraced destination-pipeline throughput baseline"}, indent=2) + "\n")
print("TRACED_READ_COMPLETED", flush=True)
