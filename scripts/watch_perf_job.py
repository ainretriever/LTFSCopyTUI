#!/usr/bin/env python3
"""Observe only persisted job state/proc metrics; never issue tape commands."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import socket
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("job_id")
    parser.add_argument("results", type=Path)
    parser.add_argument("--job-root", type=Path, default=Path.home() / ".local/state/tapecpy/jobs")
    parser.add_argument("--timeout", type=int, default=3600)
    args = parser.parse_args()
    source = args.job_root / args.job_id / "state.json"
    args.results.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + args.timeout
    previous = None
    last_print = 0.0
    with (args.results / (args.job_id + ".samples.jsonl")).open("a", encoding="utf-8") as samples:
        while time.monotonic() < deadline:
            state = json.loads(source.read_text())
            if not (args.results / (args.job_id + ".ipc.json")).exists():
                try:
                    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                        client.settimeout(5)
                        client.connect(str(source.parent / "control.sock"))
                        client.sendall(b'{"command":"status","protocol_version":1}\n')
                        wire = b""
                        while not wire.endswith(b"\n"):
                            block = client.recv(65536)
                            if not block:
                                raise RuntimeError("IPC closed without complete response")
                            wire += block
                            if len(wire) > 1024**2:
                                raise RuntimeError("IPC response exceeded 1 MiB")
                    response = json.loads(wire)
                    if response.get("response") != "state":
                        raise RuntimeError("IPC status failed: " + str(response))
                    spec = response["state"]["spec"]
                    (args.results / (args.job_id + ".ipc.json")).write_text(json.dumps({"response_bytes": len(wire), "phase": response["state"]["phase"], "preflight_in_status": "read_preflight" in spec or "write_preflight" in spec}, indent=2) + "\n")
                except (FileNotFoundError, ConnectionRefusedError):
                    pass
            progress = state["progress"]
            sample = {"observed_at": datetime.now(timezone.utc).isoformat(), "revision": state["revision"], "phase": state["phase"], "message": state["message"], "progress": {k: v for k, v in progress.items() if k != "throughput_history"}, "state_bytes": source.stat().st_size}
            pid = state.get("runner_pid")
            if pid:
                try:
                    sample["memory"] = {line.split(":", 1)[0]: line.split(":", 1)[1].strip() for line in Path(f"/proc/{pid}/status").read_text().splitlines() if line.startswith(("VmRSS:", "VmHWM:"))}
                except (FileNotFoundError, PermissionError):
                    pass
            samples.write(json.dumps(sample, ensure_ascii=False) + "\n")
            samples.flush()
            phase = state["phase"]
            if (phase, state["message"]) != previous or time.monotonic() - last_print > 20:
                print(json.dumps({"phase": phase, "message": state["message"], "bytes": progress["bytes_completed"], "tape_MB_s": round((progress.get("tape_bytes_per_second") or 0) / 1e6, 2), "buffer_bytes": progress.get("buffer_used_bytes"), "reader_waiting": progress.get("reader_waiting"), "writer_waiting": progress.get("writer_waiting")}, ensure_ascii=False), flush=True)
                previous = (phase, state["message"])
                last_print = time.monotonic()
            if phase in ("completed", "failed", "cancelled", "interrupted"):
                (args.results / (args.job_id + ".final.json")).write_text(json.dumps(state, ensure_ascii=False, indent=2) + "\n")
                if phase != "completed":
                    raise SystemExit("job did not complete: " + str(state.get("error")))
                return
            time.sleep(1)
    raise SystemExit("monitor timeout; detached job was NOT cancelled")


if __name__ == "__main__":
    main()
