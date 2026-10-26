#!/usr/bin/env python3
"""Read-only same-file long-stream diagnostic; no payload files or tape writes."""
import json
import os
from pathlib import Path
import signal
import subprocess
import time

base=Path.home()/'tapecpy-sha256-long-20261026'
entries=json.loads((base/'index-inventory/files.json').read_text())
entry=next(e for e in entries if e['path']=='/tape01/00.tar')
spec=dict(path=entry['path'],bytes=entry['length'],sha256=entry['hashes']['ltfs.hash.sha256sum'],modes=['read','serial','pipeline','pipeline','serial','read'])
out=base/'long-comparison'
out.mkdir()
(out/'spec.json').write_text(json.dumps(spec,indent=2)+'\n')
(out/'binary.sha256').write_text(subprocess.check_output(['sha256sum',str(base/'bench')],text=True))
(out/'cpu.txt').write_text(subprocess.check_output(['lscpu'],text=True))
(out/'governors.json').write_text(json.dumps({str(p):p.read_text().strip() for p in Path('/sys/devices/system/cpu').glob('cpu[0-9]*/cpufreq/scaling_governor')},indent=2))
state=dict(status='running',started_unix=time.time(),pid=None,spec=spec)
def save():
 tmp=out/'status.tmp';tmp.write_text(json.dumps(state,indent=2)+'\n');tmp.replace(out/'status.json')
save()
with (out/'test.log').open('x') as log,(out/'host-samples.jsonl').open('x') as samples:
 child=subprocess.Popen([str(base/'bench'),'app::verify_pipeline::hardware::verify_ab','--exact','--ignored','--nocapture'],env=dict(os.environ,TAPECPY_VERIFY_AB=json.dumps(spec)),stdout=log,stderr=subprocess.STDOUT)
 def stop(signum,frame):
  state['status']='stopping';save()
  if child.poll() is None: child.terminate()
 signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
 state['pid']=child.pid;save()
 try:
  while child.poll() is None:
   sample=dict(time=time.time(),frequency_khz={p.parts[-3]:p.read_text().strip() for p in Path('/sys/devices/system/cpu').glob('cpu[0-9]*/cpufreq/scaling_cur_freq')},threads={})
   for p in Path(f'/proc/{child.pid}/task').glob('*/stat'):
    try: sample['threads'][p.parent.name]=p.read_text()
    except FileNotFoundError: pass
   samples.write(json.dumps(sample)+'\n');samples.flush()
   time.sleep(1)
  state.update(status='completed' if child.returncode==0 else 'failed_or_interrupted',returncode=child.returncode,finished_unix=time.time());save()
 finally:
  if child.poll() is None: child.terminate();child.wait()
results=[]
for line in (out/'test.log').read_text().splitlines():
 if line.startswith('VERIFY_AB_RESULT='): results.append(json.loads(line.split('=',1)[1]))
(out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
with (out/'health-after.txt').open('x') as health:
 subprocess.run([str(Path.home()/'.local/bin/tapecpy'),'health','/dev/sg0'],stdout=health,stderr=subprocess.STDOUT)
