#!/usr/bin/env python3
"""Bounded restore slab capacity, paired output costs and read-only tape validation."""
import json,os,subprocess,shutil
from pathlib import Path
BASE=Path('/home/ain/tapecpy-buffer-fix-20261024')
ROOTS={'local':BASE/'outputs','nfs':Path('/mnt/tapecpy_nfs/buffer-fix-20261024'),'smb':Path('/mnt/tapecpy_smb/buffer-fix-20261024')}
for p in ROOTS.values():p.mkdir(exist_ok=True)
def run(tag,version,test,spec,proto=None):
 env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(spec))
 before=Path('/proc/fs/cifs/Stats').read_text() if proto=='smb' else None
 with (BASE/(tag+'.log')).open('w') as log:
  subprocess.run(['/usr/bin/time','-f','RESOURCE_MAXRSS_KIB=%M',str(BASE/version),'app::restore_tests::'+test,'--exact','--ignored','--nocapture'],env=env,stdout=log,stderr=log,check=True,timeout=900)
 lines=(BASE/(tag+'.log')).read_text().splitlines()
 result=next(json.loads(l.split('=',1)[1]) for l in lines if l.startswith('RESTORE_RESULT='))
 rss=next(int(l.split('=',1)[1]) for l in lines if l.startswith('RESOURCE_MAXRSS_KIB='))
 row=dict(tag=tag,version=version,result=result,maxrss_kib=rss,cifs_before=before,cifs_after=Path('/proc/fs/cifs/Stats').read_text() if proto=='smb' else None)
 with (BASE/'results.jsonl').open('a') as out:out.write(json.dumps(row)+'\n')
 print(tag,flush=True)
for size,record in [(4096,4096),(65536,65536),(1048576,524288),(4194304,524288)]:
 for v in ('before','after'):run(f'queue-{size}-{v}',v,'restore_queue',dict(size=size,record=record))
(BASE/'queue-completed.txt').write_text('queue assertions passed; cancellation only, no SHA claim\n')
for repeat in range(2):
 for size,count in [(4096,1000),(1048576,1024)]:
  for proto,parent in ROOTS.items():
   for version in (['before','after'] if repeat==0 else ['after','before']):
    tag=f'{proto}-{size}-r{repeat}-{version}';root=parent/tag
    run(tag,version,'restore_host',dict(root=str(root),size=size,count=count),proto)
    shutil.rmtree(root)
(BASE/'host-completed.txt').write_text('all paired host outputs SHA verified\n')
tag='tape-smb-1m-after';root=ROOTS['smb']/tag
run(tag,'after','restore_tape',dict(target='/restore-research-20261024/size-1048576',root=str(root)),'smb')
shutil.rmtree(root)
(BASE/'completed.txt').write_text('queue, paired host and read-only tape restore done\n')
