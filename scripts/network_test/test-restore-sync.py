#!/usr/bin/env python3
"""Paired host-output sync semantics and cost, no mount changes."""
import json,os,subprocess,time,shutil
from pathlib import Path
BASE=Path('/home/ain/tapecpy-sync-fix-20261024')
ROOTS={'local':BASE/'outputs','nfs':Path('/mnt/tapecpy_nfs/sync-fix-20261024'),'smb':Path('/mnt/tapecpy_smb/sync-fix-20261024')}
for root in ROOTS.values():root.mkdir(exist_ok=True)
for repeat in range(2):
 for size,count in [(4096,1000),(1048576,1024)]:
  for proto,parent in ROOTS.items():
   for version in (['before','after'] if repeat==0 else ['after','before']):
    tag=f'{proto}-{size}-r{repeat}-{version}';root=parent/tag
    spec=dict(root=str(root),size=size,count=count)
    env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(spec))
    before=Path('/proc/fs/cifs/Stats').read_text() if proto=='smb' else None
    with (BASE/(tag+'.log')).open('w') as log:
     subprocess.run([str(BASE/version),'app::restore_tests::restore_host','--exact','--ignored','--nocapture'],env=env,stdout=log,stderr=log,check=True,timeout=300)
    after=Path('/proc/fs/cifs/Stats').read_text() if proto=='smb' else None
    result=next(json.loads(l.split('=',1)[1]) for l in (BASE/(tag+'.log')).read_text().splitlines() if l.startswith('RESTORE_RESULT='))
    with (BASE/'results.jsonl').open('a') as out:out.write(json.dumps(dict(tag=tag,version=version,result=result,cifs_before=before,cifs_after=after))+'\n')
    shutil.rmtree(root);print(tag,flush=True)
(BASE/'host-completed.txt').write_text('all paired host outputs verified\n')
# Same on-tape 1MiB fixture used by the earlier restore baseline: read only.
tag='tape-smb-1m-after';root=ROOTS['smb']/tag
spec=dict(target='/restore-research-20261024/size-1048576',root=str(root))
env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(spec));before=Path('/proc/fs/cifs/Stats').read_text()
with (BASE/(tag+'.log')).open('w') as log:
 subprocess.run([str(BASE/'after'),'app::restore_tests::restore_tape','--exact','--ignored','--nocapture'],env=env,stdout=log,stderr=log,check=True,timeout=900)
result=next(json.loads(l.split('=',1)[1]) for l in (BASE/(tag+'.log')).read_text().splitlines() if l.startswith('RESTORE_RESULT='))
with (BASE/'results.jsonl').open('a') as out:out.write(json.dumps(dict(tag=tag,result=result,cifs_before=before,cifs_after=Path('/proc/fs/cifs/Stats').read_text()))+'\n')
shutil.rmtree(root)
(BASE/'completed.txt').write_text('host pairs and tape restore verified\n')
