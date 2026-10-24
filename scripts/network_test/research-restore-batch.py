#!/usr/bin/env python3
"""Continue after pilot: contiguous 32GiB restore preserves all four 8GiB windows."""
import json,os,subprocess,time,shutil
from pathlib import Path
BASE=Path('/home/ain/tapecpy-restore-research-20261024')
ROOTS={'local':BASE/'outputs','nfs':Path('/mnt/tapecpy_nfs/restore-research-20261024'),'smb':Path('/mnt/tapecpy_smb/restore-research-20261024')}
def call(proto,repeat):
 tag=f'batch-{proto}-r{repeat}';root=ROOTS[proto]/tag
 if (BASE/(tag+'.done')).exists():return
 spec=dict(target='/restore-research-20261024',root=str(root))
 env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(spec));start=time.time()
 with (BASE/(tag+'.log')).open('w') as log:
  subprocess.run([str(BASE/'bench'),'app::restore_tests::restore_tape','--exact','--ignored','--nocapture'],env=env,stdout=log,stderr=log,check=True,timeout=2400)
 result=next(json.loads(l.split('=',1)[1]) for l in (BASE/(tag+'.log')).read_text().splitlines() if l.startswith('RESTORE_RESULT='))
 assert result['verified'] and result['bytes']==32*1024**3
 with (BASE/'results.jsonl').open('a') as f:f.write(json.dumps(dict(tag=tag,start_unix=start,wall_seconds=time.time()-start,result=result))+'\n')
 (BASE/(tag+'.done')).write_text('all files verified\n');print(tag,flush=True)
 shutil.rmtree(root)
# One complete mixed restore per target, with per-size 8GiB windows.
for proto in ['local','nfs','smb']:call(proto,0)
with (BASE/'final-diagnose.txt').open('w') as f:subprocess.run(['/home/ain/.local/bin/tapecpy','diagnose','/dev/sg0'],stdout=f,stderr=f,check=True,timeout=300)
with (BASE/'final-health.txt').open('w') as f:subprocess.run(['/home/ain/.local/bin/tapecpy','health','/dev/sg0'],stdout=f,stderr=f,check=True,timeout=60)
(BASE/'completed.txt').write_text('host, pilots and three complete 32GiB restores verified\n')
