#!/usr/bin/env python3
"""Sequential restore research; new output dirs, validated before cleanup."""
import json,os,subprocess,time,shutil,hashlib
from pathlib import Path
BASE=Path('/home/ain/tapecpy-restore-research-20261024')
ROOTS={'local':BASE/'outputs','nfs':Path('/mnt/tapecpy_nfs/restore-research-20261024'),'smb':Path('/mnt/tapecpy_smb/restore-research-20261024')}
for p in ROOTS.values():p.mkdir(exist_ok=True)
FIXTURE='/restore-research-20261024'
def call(test,spec,tag,timeout=1800,fixture=False):
 if (BASE/(tag+'.done')).exists():return
 env=dict(os.environ,TAPECPY_RESTORE_SPEC=json.dumps(spec))
 if fixture:env['TAPECPY_RESTORE_FIXTURE']='1'
 cmd=[str(BASE/'bench'),'app::restore_tests::'+test,'--exact','--ignored','--nocapture']
 start=time.time()
 with (BASE/(tag+'.log')).open('w') as log:
  r=subprocess.run(cmd,env=env,stdout=log,stderr=log,timeout=timeout)
 r.check_returncode()
 content=(BASE/(tag+'.log')).read_text()
 result=next(json.loads(l.split('=',1)[1]) for l in content.splitlines() if l.startswith('RESTORE_RESULT='))
 with (BASE/'results.jsonl').open('a') as f:f.write(json.dumps({'tag':tag,'start_unix':start,'wall_seconds':time.time()-start,'result':result})+'\n')
 (BASE/(tag+'.done')).write_text('verified benchmark complete\n')
 print(tag,flush=True)
 if 'root' in spec:
  root=Path(spec['root']);assert root.parent in ROOTS.values()
  shutil.rmtree(root)
def host(proto,size,count,tag,**kw):
 call('restore_host',dict(root=str(ROOTS[proto]/tag),size=size,count=count,**kw),tag)
# Quick real pipeline screening (1GiB each); protocol order rotates.
for repeat in range(3):
 protocols=['local','nfs','smb'];protocols=protocols[repeat:]+protocols[:repeat]
 for size in [1048576,4194304,16777216,67108864]:
  for proto in protocols:host(proto,size,1024**3//size,f'host-{proto}-{size}-r{repeat}')
# Handle cache boundary: same per-file work, 4KiB and 1MiB files.
for size in [4096,1048576]:
 for count in [255,256,257,1024]:
  for proto in ROOTS:host(proto,size,count,f'handles-{proto}-{size}-{count}')
# Quantify instrumentation overhead, warm local 1GiB runs.
for repeat in range(2):
 for enabled in ([False,True] if repeat==0 else [True,False]):
  host('local',1048576,1024,f'overhead-{repeat}-{enabled}',instrument=enabled)
for size,record in [(4096,4096),(65536,65536),(1048576,524288),(4194304,524288)]:
 call('restore_queue',dict(size=size,record=record),f'queue-{size}-{record}')
# Steady target writes: 8GiB per run, 1MiB and64MiB; not simultaneous protocols.
for size in [1048576,67108864]:
 for proto in ROOTS:host(proto,size,8*1024**3//size,f'steady-{proto}-{size}')
(BASE/'host-completed.txt').write_text('host pipeline and thresholds complete\n')
# A single append-only 32GiB fixture; all tape access under exclusive lease.
call('restore_fixture',dict(target=FIXTURE),'fixture',3600,True)
for size in [1048576,4194304,16777216,67108864]:
 target=f'{FIXTURE}/size-{size}'
 call('restore_tape',dict(target=target,**{'null':True}),f'null-{size}')
 for proto in ROOTS:
  tag=f'tape-{proto}-{size}-r0'
  call('restore_tape',dict(target=target,root=str(ROOTS[proto]/tag)),tag)
# Repeat MB focus with reverse target order, separating run-order effects.
for size in [4194304,1048576]:
 for proto in ['smb','nfs','local']:
  tag=f'tape-{proto}-{size}-r1'
  call('restore_tape',dict(target=f'{FIXTURE}/size-{size}',root=str(ROOTS[proto]/tag)),tag)
with (BASE/'final-diagnose.txt').open('w') as out:
 subprocess.run(['/home/ain/.local/bin/tapecpy','diagnose','/dev/sg0'],stdout=out,stderr=out,check=True,timeout=300)
with (BASE/'final-health.txt').open('w') as out:
 subprocess.run(['/home/ain/.local/bin/tapecpy','health','/dev/sg0'],stdout=out,stderr=out,check=True,timeout=60)
(BASE/'completed.txt').write_text('all restore research completed\n')
