#!/usr/bin/env python3
"""Tracing controls after throughput runs; not mixed into timing samples."""
import json,os,subprocess,time,shutil
from pathlib import Path
base=Path('/home/ain/tapecpy-restore-research-20261024')
roots={'local':base/'outputs','nfs':Path('/mnt/tapecpy_nfs/restore-research-20261024'),'smb':Path('/mnt/tapecpy_smb/restore-research-20261024')}
while not (base/'extra-completed.txt').exists():time.sleep(5)
for proto,parent in roots.items():
 tag=f'trace-pipeline-{proto}';root=parent/tag
 env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(dict(root=str(root),size=4096,count=1000,record=4096)))
 with (base/(tag+'.stdout')).open('w') as out,(base/(tag+'.trace')).open('w') as err:
  subprocess.run(['strace','-f','-c','-w','-e','trace=openat,openat2,close,write,lseek,fsync,fdatasync,syncfs',str(base/'bench'),'app::restore_tests::restore_host','--exact','--ignored','--nocapture'],env=env,stdout=out,stderr=err,check=True,timeout=180)
 result=next(json.loads(l.split('=',1)[1]) for l in (base/(tag+'.stdout')).read_text().splitlines() if l.startswith('RESTORE_RESULT='));assert result['verified']
 shutil.rmtree(root)
(base/'trace-completed.txt').write_text('all three syscall control outputs verified\n')
