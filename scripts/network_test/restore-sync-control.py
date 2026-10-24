#!/usr/bin/env python3
"""Small synchronization semantics control, isolated from throughput tests."""
import ctypes,json,os,time
from pathlib import Path
base=Path('/home/ain/tapecpy-restore-research-20261024')
root=Path('/mnt/tapecpy_smb/restore-research-20261024/sync-control')
root.mkdir()
libc=ctypes.CDLL(None,use_errno=True)
anchor=os.open(root,os.O_RDONLY|os.O_DIRECTORY)
rows=[]
for fsync in [False,True]:
 path=root/('explicit-fsync' if fsync else 'close-syncfs')
 before=Path('/proc/fs/cifs/Stats').read_text();start=time.monotonic()
 fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
 os.write(fd,os.urandom(65536))
 if fsync:os.fsync(fd)
 os.close(fd)
 rc=libc.syncfs(anchor)
 if rc:raise OSError(ctypes.get_errno(),'syncfs')
 after=Path('/proc/fs/cifs/Stats').read_text()
 rows.append(dict(explicit_fsync=fsync,seconds=time.monotonic()-start,before=before,after=after))
os.close(anchor)
(base/'sync-control.json').write_text(json.dumps(rows,indent=2)+'\n')
for path in root.iterdir():path.unlink()
root.rmdir()
print('sync control complete')
