# NFS / SMB 测试环境准备

服务端 reactor：SSH 管理地址 192.168.10.235（enp34s0，1GbE）；数据地址 192.168.10.239（enp38s0，10GbE，MAC f4:52:14:d6:06:c6）。磁带机 tapeserver：192.168.10.125，enp3s0，2.5GbE。主机命令读取的协商速率与用户说明一致。

初次检查发现双网卡同网段导致回程走千兆口。用户随后拔掉 .235 的千兆网线；实测 enp34s0 carrier=0，到 .125 和默认外网路由均已切换到 enp38s0/.239，SSH .239 正常。部署脚本现已移除源地址策略路由和客户端静态邻居设置。

用户确认 .228 不在线，明确要求退役 `/srv/furaffinity/incoming` 的 NFS 登记。已准备 `remove-old-nfs.sh`，由服务端部署脚本的 `--remove-old-nfs` 参数调用：备份并移除旧导出配置的指定记录，停止 NFS/mountd 后清理 rmtab 内匹配的旧客户端记录，再恢复服务。目录和数据不删除，其他记录保留。用户随后已执行两端部署脚本；实测旧导出与历史挂载登记已清除，目录数据保留。

测试新增 `/etc/exports.d/tapecpy-perf.exports`，导出 `/srv/tapecpy-perf`，仅允许 .125，使用 rw/sync/root_squash。目录在系统 NVMe/ext4 上（初始约 780GiB 可用），不涉及已有的 `/mnt/fa-recovery`。

SMB 原先没有运行，现有配置仅有发行版默认打印共享。脚本备份原配置并添加独立 include，将 SMB 绑定 .239 和 loopback，限制 .125 访问；共享名 `tapecpy-perf`。使用专用 `tapecpybench` 账号，随机口令保存于服务端用户私有目录的 `smb.credentials`，不打印到日志；随后由 agent 通过 SSH 管道传到 tapeserver 同名私有目录。不更改 ain 的登录密码，不启用 guest。

客户端采用 `/run/systemd/system/` 中的临时 automount/mount 单元，挂载点 `/mnt/tapecpy_nfs` 和 `/mnt/tapecpy_smb`，不修改 fstab。NFS 使用 4.2/TCP/hard，SMB 使用 3.1.1/cache=strict。SMB automount 在凭据传输完成后才首次访问。两协议使用同一服务端测试目录，测试串行执行。

## 用户执行步骤

两机 `sudo -n true` 均报告需要交互认证。部署脚本已上传并通过 bash 语法检查，用户已在终端完成交互 sudo 执行。以下命令作为部署记录：

```sh
ssh -t ain@192.168.10.239 'sudo bash /home/ain/tapecpy-network-test/setup-server.sh --remove-old-nfs'
ssh -t ain@192.168.10.125 'sudo bash /home/ain/tapecpy-network-test/setup-client.sh'
```

不需要向 agent 提供密码或开放无限制免密 sudo。服务端脚本会安装 nfs-kernel-server、samba、iperf3；客户端安装 nfs-common、cifs-utils、iperf3。如 UFW 已启用，服务端仅新增从 .125 经 enp38s0 到 .239 的 TCP 2049、445、5201 规则。

脚本源码：`scripts/network_test/setup-server.sh`、`setup-client.sh`、`remove-old-nfs.sh`。服务和导出配置保留至主动清理；临时 automount 在重启后失效。旧 NFS 备份保存在 `/var/backups/tapecpy-nfs/`。

## 后续验证

1. 私密传输 SMB 凭据，确认实际挂载来源、版本、选项、读写权限。
2. 双向 iperf3 和两端网卡计数确认真实数据路径，分开记录网络上限与存储/共享吞吐。
3. 从现有不可压缩数据集构建网络源，校验副本 SHA-256；进行无磁带的顺序读取/写入基线。
4. 分别从 NFS、SMB 源写入磁带，再恢复到各自的新目录，独立验证完整性；记录源/目标等待、缓冲、吞吐和原始通道计数，避免把网络不足误归因于磁带。

配置依据：[Ubuntu NFS 文档](https://ubuntu.com/server/docs/how-to/networking/install-nfs/)、[Ubuntu Samba 文档](https://documentation.ubuntu.com/server/how-to/samba/file-server/index.html)、[Samba smb.conf 手册](https://www.samba.org/samba/docs/current/man-html/smb.conf.5)。实际软件版本和协商参数以部署后验证为准。

部署后的实测记录见 `NETWORK_TEST_RESULTS_2026-10-23.md`。
