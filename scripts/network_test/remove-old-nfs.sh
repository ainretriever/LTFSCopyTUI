#!/usr/bin/env bash
# Remove only the retired export and its stale .228 mount registration.
set -euo pipefail
[[ $EUID == 0 && $(hostname) == reactor ]] || exit 1
python3 - <<'PY'
from pathlib import Path
p = Path('/etc/exports.d/furaffinity.exports')
if p.exists():
    entries = [line for line in p.read_text().splitlines() if line.strip() and not line.lstrip().startswith('#')]
    for line in entries:
        fields = line.split()
        if len(fields) != 2 or fields[0] != '/srv/furaffinity/incoming' or not fields[1].startswith('192.168.10.228('):
            raise SystemExit('Old export configuration changed; review before removal.')
PY
bench_was_active=no
systemctl is-active --quiet nfs-server.service && bench_was_active=yes
systemctl stop nfs-server.service nfs-mountd.service
trap 'if [[ $bench_was_active == yes ]]; then systemctl start nfs-server.service; fi' EXIT
python3 - <<'PY'
from datetime import datetime, timezone
import os
from pathlib import Path
import shutil
import tempfile

backup = Path('/var/backups/tapecpy-nfs') / datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S.%fZ')
backup.mkdir(parents=True, mode=0o700)
for name in ('/etc/exports.d/furaffinity.exports', '/var/lib/nfs/rmtab'):
    path = Path(name)
    if not path.exists():
        continue
    shutil.copy2(path, backup / path.name)
    lines = path.read_text().splitlines(keepends=True)
    if path.name == 'furaffinity.exports':
        kept = [line for line in lines if not line.split() or line.split()[0] != '/srv/furaffinity/incoming']
    else:
        kept = [line for line in lines if not line.startswith('192.168.10.228:/srv/furaffinity/incoming:')]
    st = path.stat()
    fd, temporary = tempfile.mkstemp(prefix='.tapecpy-', dir=path.parent)
    try:
        os.fchmod(fd, st.st_mode & 0o777)
        os.fchown(fd, st.st_uid, st.st_gid)
        with os.fdopen(fd, 'w') as stream:
            stream.writelines(kept)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
print('Backup:', backup)
PY
systemctl daemon-reload
if [[ $bench_was_active == yes ]]; then
    systemctl start nfs-server.service
    showmount -a localhost
fi
echo 'Retired NFS export/registration removed; directory and data preserved.'
