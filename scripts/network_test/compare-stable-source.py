#!/usr/bin/env python3
"""Read-only paired experiment; no cache/mount changes or tape access."""
import json,os,subprocess
from pathlib import Path
BASE=Path('/home/ain/tapecpy-stable-source-20261024')
ROOTS={'local':Path('/home/ain/tapecpy-small-20261024/local'),'nfs':Path('/mnt/tapecpy_nfs/small-20261024'),'smb':Path('/mnt/tapecpy_smb/small-20261024')}
def call(spec,tag,trace=False):
 env=dict(os.environ,TAPECPY_HOST_BENCH_SPEC=json.dumps(spec))
 cmd=[str(BASE/'bench'),'app::performance_tests::host_small_files','--exact','--ignored','--nocapture']
 if trace:cmd=['strace','-f','-c','-e','trace=statx,pread64,openat,close']+cmd
 r=subprocess.run(cmd,env=env,capture_output=True,text=True,timeout=180)
 (BASE/(tag+'.log')).write_text(r.stdout+r.stderr);r.check_returncode()
 row=next(json.loads(l.split('=',1)[1]) for l in r.stdout.splitlines() if l.startswith('BENCH_RESULT='))
 with (BASE/'results.jsonl').open('a') as f:f.write(json.dumps({'tag':tag,'result':row})+'\n')
 print(tag,flush=True)
for round in range(4):
 for size,count,nested in [(4096,10000,False),(4096,10000,True),(65536,2048,False)]:
  for proto,root in ROOTS.items():
   for stable in ([False,True] if round%2==0 else [True,False]):
    spec=dict(mode='source',root=str(root/('source-'+str(size)+'-'+('nested' if nested else 'flat'))),size=size,count=count,nested=nested,assume_stable=stable)
    call(spec,f'{proto}-{size}-{nested}-r{round}-stable{stable}')
for proto in ('nfs','smb'):
 for stable in (False,True):
  call(dict(mode='source',root=str(ROOTS[proto]/'source-4096-nested/group-000'),size=4096,count=100,nested=False,assume_stable=stable),f'trace-{proto}-{stable}',True)
(BASE/'completed.txt').write_text('all source hashes verified; no tape access\n')
