#!/usr/bin/env python3
"""Post-run controls; sequential, no tape access, new destination directories."""
import json,os,subprocess,shutil,time
from pathlib import Path
BASE=Path('/home/ain/tapecpy-restore-research-20261024')
ROOTS={'local':BASE/'outputs','nfs':Path('/mnt/tapecpy_nfs/restore-research-20261024'),'smb':Path('/mnt/tapecpy_smb/restore-research-20261024')}
# Run independently only after main study; importing its module would execute it.
while not (BASE/'completed.txt').exists():time.sleep(5)
for proto,root in ROOTS.items():
 for size,count,nested,record,label in [(1048576,1024,True,524288,'nested-1m'),(1048576,8192,True,524288,'nested-1m-8g'),(67108864,16,True,524288,'nested-64m'),(1048576,1024,False,4096,'record-4k'),(1048576,1024,False,65536,'record-64k')]:
  tag=f'extra-{proto}-{label}';target=root/tag
  spec=dict(root=str(target),size=size,count=count,nested=nested,record=record)
  env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(spec));start=time.time()
  with (BASE/(tag+'.log')).open('w') as f:
   subprocess.run([str(BASE/'bench'),'app::restore_tests::restore_host','--exact','--ignored','--nocapture'],env=env,stdout=f,stderr=f,check=True,timeout=600)
  result=next(json.loads(l.split('=',1)[1]) for l in (BASE/(tag+'.log')).read_text().splitlines() if l.startswith('RESTORE_RESULT='))
  with (BASE/'extra-results.jsonl').open('a') as f:f.write(json.dumps(dict(tag=tag,start_unix=start,result=result))+'\n')
  shutil.rmtree(target);print(tag,flush=True)
(BASE/'extra-completed.txt').write_text('nested and record-size controls verified\n')
