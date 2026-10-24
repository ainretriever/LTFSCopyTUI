#!/usr/bin/env python3
"""Check instrumentation cost at high message rates after other controls."""
import json,os,subprocess,time,shutil
from pathlib import Path
base=Path('/home/ain/tapecpy-restore-research-20261024')
roots={'local':base/'outputs','nfs':Path('/mnt/tapecpy_nfs/restore-research-20261024'),'smb':Path('/mnt/tapecpy_smb/restore-research-20261024')}
while not (base/'actual-queue-completed.txt').exists():time.sleep(2)
for proto,parent in roots.items():
 for enabled in [False,True]:
  tag=f'calibration-4k-{proto}-{enabled}';root=parent/tag
  spec=dict(root=str(root),size=1048576,count=1024,record=4096,instrument=enabled)
  env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(spec));start=time.time()
  with (base/(tag+'.log')).open('w') as log:subprocess.run([str(base/'bench'),'app::restore_tests::restore_host','--exact','--ignored','--nocapture'],env=env,stdout=log,stderr=log,check=True,timeout=300)
  result=next(json.loads(l.split('=',1)[1]) for l in (base/(tag+'.log')).read_text().splitlines() if l.startswith('RESTORE_RESULT='));assert result['verified']
  with (base/'calibration-results.jsonl').open('a') as out:out.write(json.dumps(dict(tag=tag,start_unix=start,result=result))+'\n')
  shutil.rmtree(root);print(tag,flush=True)
(base/'calibration-completed.txt').write_text('high-rate instrumentation controls verified\n')
