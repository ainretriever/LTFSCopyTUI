#!/usr/bin/env bash
# Dedicated test export/share on reactor; run with sudo after reviewing.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Run with sudo.' >&2; exit 1; }
[[ $(hostname) == reactor && $(id -u ain) == 1000 ]] || exit 1
ip -4 address show dev enp38s0 | grep -q '192.168.10.239/24'
if [[ ${1:-} == --remove-old-nfs ]]; then
    bench_script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    bash "$bench_script_dir/remove-old-nfs.sh"
fi
bench_home=/home/ain/tapecpy-network-test
bench_share=/srv/tapecpy-perf
install -d -m 700 -o ain -g ain "$bench_home"
if [[ -e $bench_share && ! -f $bench_share/.tapecpy-test ]]; then
    echo "Refusing an existing unmanaged directory: $bench_share" >&2
    exit 1
fi
[[ $(cat /sys/class/net/enp34s0/carrier) == 0 ]] || {
    echo 'Disconnect the retired 1GbE link before this simplified setup.' >&2; exit 1;
}
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends nfs-kernel-server samba iperf3
install -d -m 0770 -o ain -g ain "$bench_share"
touch "$bench_share/.tapecpy-test"
cat > /etc/exports.d/tapecpy-perf.exports <<'EOF'
/srv/tapecpy-perf 192.168.10.125(rw,sync,no_subtree_check,root_squash)
EOF
exportfs -ra
systemctl start nfs-server

# This host had only the distribution sample SMB shares before this setup.
# Keep that file and add the test configuration through a single include.
if ! grep -qF 'include = /etc/samba/tapecpy-perf.conf' /etc/samba/smb.conf; then
    cp -a /etc/samba/smb.conf "$bench_home/smb.conf.before-test"
    printf '\n# tapecpy network performance test\n[global]\ninclude = /etc/samba/tapecpy-perf.conf\n' >> /etc/samba/smb.conf
fi
if ! id tapecpybench >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin tapecpybench
fi
cat > /etc/samba/tapecpy-perf.conf <<'EOF'
[global]
    interfaces = lo 192.168.10.239/32
    bind interfaces only = yes
    smb ports = 445
    server min protocol = SMB3_00
    security = user
    map to guest = Never
    hosts allow = 127.0.0.1 192.168.10.125
    hosts deny = ALL
    disable netbios = yes
    load printers = no
    disable spoolss = yes
    usershare allow guests = no
[tapecpy-perf]
    path = /srv/tapecpy-perf
    read only = no
    browseable = no
    guest ok = no
    valid users = tapecpybench
    force user = ain
    force group = ain
    create mask = 0660
    directory mask = 0770
EOF
umask 077
if [[ ! -f $bench_home/smb.credentials ]]; then
    bench_password=$(python3 -c 'import secrets; print(secrets.token_hex(32))')
    printf 'username=tapecpybench\npassword=%s\n' "$bench_password" > "$bench_home/smb.credentials"
else
    bench_password=$(sed -n 's/^password=//p' "$bench_home/smb.credentials")
    [[ -n $bench_password ]] || exit 1
fi
printf '%s\n%s\n' "$bench_password" "$bench_password" | smbpasswd -s -a tapecpybench >/dev/null
unset bench_password
chown ain:ain "$bench_home/smb.credentials"
chmod 600 "$bench_home/smb.credentials"
testparm -s >/dev/null
systemctl restart smbd

# Only the 10GbE link remains; no policy routing is needed.
if command -v ufw >/dev/null && ufw status | grep -q '^Status: active'; then
    for bench_port in 2049 445 5201; do
        ufw allow in on enp38s0 proto tcp from 192.168.10.125 to 192.168.10.239 port "$bench_port" comment tapecpy-perf
    done
fi
ip route get 192.168.10.125 from 192.168.10.239
exportfs -v
touch "$bench_home/server-ready"
chown ain:ain "$bench_home/server-ready"
echo 'SERVER_READY; credentials saved privately (not printed).'
