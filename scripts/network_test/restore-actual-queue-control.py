#!/usr/bin/env python3
import json,os,subprocess,time
from pathlib import Path
base=Path('/home/ain/tapecpy-restore-research-20261024')
while not (base/'trace-completed.txt').exists():time.sleep(5)
for size,record in [(4096,4096),(65536,65536),(1048576,524288),(4194304,524288)]:
 tag=f'queue-actual-{size}-{record}'
 env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(dict(size=size,record=record)))
 with (base/(tag+'.log')).open('w') as log:
  subprocess.run([str(base/'bench'),'app::restore_tests::restore_queue','--exact','--ignored','--nocapture'],env=env,stdout=log,stderr=log,check=True,timeout=60)
 result=next(json.loads(l.split('=',1)[1]) for l in (base/(tag+'.log')).read_text().splitlines() if l.startswith('RESTORE_RESULT='))
 with (base/'actual-queue-results.jsonl').open('a') as out:out.write(json.dumps(dict(tag=tag,result=result))+'\n')
 print(tag,flush=True)
(base/'actual-queue-completed.txt').write_text('actual spawn capacity and cancellation controls complete\n')
