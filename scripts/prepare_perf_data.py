#!/usr/bin/env python3
"""Generate allocated, incompressible tape fixtures and an independent SHA-256 manifest."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--large-gib", type=int, default=8)
    parser.add_argument("--small-files", type=int, default=10000)
    args = parser.parse_args()
    if args.large_gib < 1 or args.small_files < 0:
        parser.error("large-gib must be positive; small-files must be nonnegative")
    root = args.destination.expanduser().absolute()
    root.parent.mkdir(parents=True, exist_ok=True)
    required = args.large_gib * 1024**3 + args.small_files * 4096 + 116 * 1024**2
    if shutil.disk_usage(root.parent).free < required + 1024**3:
        parser.error("not enough free disk space (including 1 GiB reserve)")
    root.mkdir()  # Fail rather than overwrite an existing dataset.
    started = time.monotonic()
    count = 0
    total = 0
    with (root / "manifest.jsonl").open("x", encoding="utf-8") as manifest:
        def generate(relative, size):
            nonlocal count, total
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            digest = hashlib.sha256()
            with path.open("xb") as output:
                remaining = size
                while remaining:
                    data = os.urandom(min(8 * 1024**2, remaining))
                    output.write(data)
                    digest.update(data)
                    remaining -= len(data)
                if size >= 1024**3:
                    output.flush()
                    os.fsync(output.fileno())
            manifest.write(json.dumps({"path": relative, "bytes": size, "sha256": digest.hexdigest()}, ensure_ascii=False) + "\n")
            count += 1
            total += size
        generate("large/stream.bin", args.large_gib * 1024**3)
        for number in range(args.small_files):
            generate(f"small/group-{number // 1000:02}/file-{number:06}.bin", 4096)
        for number in range(100):
            generate(f"mixed/medium-{number:03}.bin", 1024**2)
        for number in range(16):
            generate(f"mixed/nested/混合-{number:02}.bin", 1024**2)
        for number, size in enumerate([0, 1, 4095, 524287, 524288, 524289, 1048577]):
            generate(f"mixed/boundary-{number}.bin", size)
        manifest.flush()
        os.fsync(manifest.fileno())
    print(json.dumps({"root": str(root), "files": count, "bytes": total, "generation_seconds": round(time.monotonic() - started, 3)}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
