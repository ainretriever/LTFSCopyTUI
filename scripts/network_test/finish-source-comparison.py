#!/usr/bin/env python3
"""Fourth balanced host round, syscall-shape probes, and final read-only diagnosis."""
import json
import os
from pathlib import Path
import subprocess
import time

BASE=Path('/home/ain/tapecpy-source-opt-20261024')
ROOTS={'local':Path('/home/ain/tapecpy-small-20261024/local'),
       'nfs':Path('/mnt/tapecpy_nfs/small-20261024'),
       'smb':Path('/mnt/tapecpy_smb/small-20261024')}


def call(version, spec, tag):
    env=dict(os.environ,TAPECPY_HOST_BENCH_SPEC=json.dumps(spec))
    result=subprocess.run([str(BASE/version),'app::performance_tests::host_small_files',
                           '--exact','--ignored','--nocapture'],env=env,capture_output=True,
                          text=True,timeout=300)
    (BASE/(tag+'.log')).write_text(result.stdout+result.stderr)
    result.check_returncode()
    row=next(json.loads(line.split('=',1)[1]) for line in result.stdout.splitlines()
             if line.startswith('BENCH_RESULT='))
    with (BASE/'results.jsonl').open('a') as output:
        output.write(json.dumps(dict(tag=tag,version=version,result=row))+'\n')
    print(tag,flush=True)


def main():
    deadline=time.monotonic()+3600
    while not (BASE/'all-completed.txt').exists():
        if time.monotonic()>deadline:
            raise RuntimeError('initial comparison did not complete')
        time.sleep(10)
    # Two AB and two BA rounds balance the ordering of client-cache effects.
    for size,count in [(1048576,256),(65536,2048),(4096,10000)]:
        for nested in [False,True]:
            case=f'{size}-'+('nested' if nested else 'flat')
            for protocol in ROOTS:
                spec=dict(mode='source',root=str(ROOTS[protocol]/('source-'+case)),
                          size=size,count=count,nested=nested)
                for version in ['after','before']:
                    call(version,spec,f'host-{protocol}-{case}-r3-{version}')
    for protocol in ['nfs','smb']:
        for version in ['before','after']:
            root=ROOTS[protocol]/'source-4096-nested/group-000'
            spec=dict(mode='source',root=str(root),size=4096,count=100,nested=False)
            env=dict(os.environ,TAPECPY_HOST_BENCH_SPEC=json.dumps(spec))
            with (BASE/f'trace-{protocol}-{version}.log').open('x') as output:
                subprocess.run(['strace','-f','-e','trace=statx,read,pread64','-s','0',
                                '-o',str(BASE/f'trace-{protocol}-{version}.txt'),
                                str(BASE/version),'app::performance_tests::host_small_files',
                                '--exact','--ignored','--nocapture'],env=env,
                               stdout=output,stderr=output,check=True,timeout=60)
    with (BASE/'final-diagnose.txt').open('x') as output:
        subprocess.run(['/home/ain/.local/bin/tapecpy','diagnose','/dev/sg0'],
                       stdout=output,stderr=output,check=True,timeout=300)
    (BASE/'verified-completed.txt').write_text('balanced four-round comparison and diagnosis completed\n')


if __name__=='__main__':
    main()
