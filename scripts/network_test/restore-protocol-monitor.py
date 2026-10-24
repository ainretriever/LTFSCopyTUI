#!/usr/bin/env python3
"""Read-only timestamped mount-level counters; includes verification I/O."""
import json,time
from pathlib import Path
base=Path('/home/ain/tapecpy-restore-research-20261024')
with (base/'protocol-timeline.jsonl').open('a',buffering=1) as f:
 for _ in range(2700):
  row={'unix':time.time()}
  for name,path in [('nfs','/proc/self/mountstats'),('cifs','/proc/fs/cifs/Stats'),('memory','/proc/meminfo')]:
   try:row[name]=Path(path).read_text()
   except OSError as error:row[name]=str(error)
  f.write(json.dumps(row)+'\n')
  if (base/'completed.txt').exists():break
  time.sleep(2)
