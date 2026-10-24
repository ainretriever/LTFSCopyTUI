#!/usr/bin/env python3
"""Verify restored payloads against the independently generated manifest."""
import argparse
import hashlib
import json
from pathlib import Path
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("selection", help="source-relative file or directory, e.g. small or large/stream.bin")
    parser.add_argument("restored", type=Path)
    args = parser.parse_args()
    selection = args.selection.strip("/")
    entries = [json.loads(line) for line in args.manifest.read_text().splitlines()]
    entries = [entry for entry in entries if entry["path"] == selection or entry["path"].startswith(selection + "/")]
    if not entries:
        raise SystemExit("manifest selection is empty")
    is_file = len(entries) == 1 and entries[0]["path"] == selection
    expected = set()
    total = 0
    started = time.monotonic()
    for entry in entries:
        relative = entry["path"][len(selection) + 1:]
        expected.add(relative)
        path = args.restored if is_file else args.restored / relative
        if path.stat().st_size != entry["bytes"]:
            raise SystemExit("length mismatch: " + str(path))
        digest = hashlib.sha256()
        with path.open("rb") as source:
            while data := source.read(8 * 1024**2):
                digest.update(data)
        if digest.hexdigest() != entry["sha256"]:
            raise SystemExit("SHA-256 mismatch: " + str(path))
        total += entry["bytes"]
    if not is_file:
        actual = {str(path.relative_to(args.restored)) for path in args.restored.rglob("*") if path.is_file()}
        if actual != expected:
            raise SystemExit("restored file list differs from manifest")
    print(json.dumps({"verified_files": len(entries), "verified_bytes": total, "seconds": round(time.monotonic() - started, 3), "sha256": "all matched"}), flush=True)


if __name__ == "__main__":
    main()
