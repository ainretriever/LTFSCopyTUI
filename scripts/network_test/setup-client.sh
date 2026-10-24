#!/usr/bin/env bash
# Install clients and temporary systemd automounts; run on tapeserver with sudo.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Run with sudo.' >&2; exit 1; }
[[ $(hostname) == tapeserver && $(id -u ain) == 1000 ]] || exit 1
ip -4 address show dev enp3s0 | grep -q '192.168.10.125/24'
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends nfs-common cifs-utils iperf3
install -d -m 700 -o ain -g ain /home/ain/tapecpy-network-test

# The server's 1GbE cable is disconnected; ordinary ARP is sufficient.
for bench_protocol in nfs smb; do
    bench_path=/mnt/tapecpy_${bench_protocol}
    bench_unit=mnt-tapecpy_${bench_protocol}
    if mountpoint -q "$bench_path"; then
        echo "Already mounted: $bench_path; review it before re-running." >&2; exit 1
    fi
    install -d -m 0755 "$bench_path"
    if [[ $bench_protocol == nfs ]]; then
        bench_what=192.168.10.239:/srv/tapecpy-perf
        bench_type=nfs
        bench_options=vers=4.2,proto=tcp,hard,rsize=1048576,wsize=1048576
    else
        bench_what=//192.168.10.239/tapecpy-perf
        bench_type=cifs
        bench_options=vers=3.1.1,credentials=/home/ain/tapecpy-network-test/smb.credentials,uid=1000,gid=1000,cache=strict
    fi
    cat > "/run/systemd/system/${bench_unit}.mount" <<EOF
[Unit]
Description=tapecpy ${bench_protocol} network test mount
[Mount]
What=${bench_what}
Where=${bench_path}
Type=${bench_type}
Options=${bench_options}
TimeoutSec=90
EOF
    cat > "/run/systemd/system/${bench_unit}.automount" <<EOF
[Unit]
Description=tapecpy ${bench_protocol} network test automount
[Automount]
Where=${bench_path}
TimeoutIdleSec=0
EOF
done
systemctl daemon-reload
systemctl start mnt-tapecpy_nfs.automount mnt-tapecpy_smb.automount
echo 'CLIENT_READY; temporary automounts expire on reboot.'
echo 'SMB becomes usable after its private credentials file is transferred.'
