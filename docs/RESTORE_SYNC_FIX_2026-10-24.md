# 恢复输出同步语义修复（2026-10-24）

## 行为与边界

此前恢复在关闭全部输出文件后调用一次 `syncfs`。上一轮协议控制证明，这在当前 CIFS 挂载上没有发送 SMB FLUSH，不能作为打开文件句柄上 fsync 的替代。本次修复位于 host output 管线，不改变唯一磁带 owner、物理 extent 顺序、队列预算、网络挂载或并发数。

- 每个文件最后一个 extent 写完后，在仍打开的文件句柄上 `sync_all`，随后显式检查 Linux `close`，全部成功才增加完成文件数。close 只调用一次，不在 EINTR 后重试。
- 多 extent 文件不在每个 extent 上同步；中途因句柄预算关闭时也检查 close 错误。空文件和纯稀疏文件在 prepare 中设置长度、同步并关闭。
- 所有文件排空后，按子目录到父目录同步涉及的目录，再同步目标根目录，最后保留 `syncfs`。只有这些步骤全部成功才允许 Completed。
- writer 的真实同步/写入错误优先于队列断开错误返回，避免隐藏失败原因。

同步保证以操作系统、挂载配置和服务器对同步调用的承诺为边界。当前 SMB 未使用 nostrictsync，服务器 strict sync=Yes。CIFS 的目录 fsync 不等于额外逐文件 FLUSH；没有做断电实验，也不声称验证了服务器硬件的断电持久性。

## 验证设计

在 tapeserver 192.168.10.125 上使用同一测试入口的修复前后 release 测试二进制，真实 ReadDestinationPipeline、512KiB record、单 writer。每个目标（本地/NFS/SMB）、大小（4KiB×1000、1MiB×1024）各做两轮，顺序 AB/BA，共24项；每项计时外逐文件 SHA 验证。未清缓存或修改挂载。网络共享仍位于 .239 的 NVMe ext4/LVM，并非机械盘。

表中为两轮总秒数的中位数，包含目标检查、预建、提交、排空和同步，不包含内容复核。样本很少，百分比仅用于描述本轮成本，不作为统计显著性结论。

|目标|文件大小/数量|修复前秒|修复后秒|变化|
|---|---|---:|---:|---:|
|local|4KiB×1000|0.0416|1.0035|+2314%|
|NFS|4KiB×1000|2.1760|2.1811|+0.2%|
|SMB|4KiB×1000|1.0880|1.8000|+65.4%|
|local|1MiB×1024|1.1053|2.0478|+85.3%|
|NFS|1MiB×1024|6.8931|6.9237|+0.4%|
|SMB|1MiB×1024|6.1127|7.2919|+19.3%|

每项修复后 file_sync 次数等于文件数。SMB 四项修复前 FLUSH 增量均0；修复后两项4KiB各1000，两项1MiB各1024，失败计数增量均0。原始 CIFS before/after 计数随数据保留。NFS close 已承担较强的写回工作，本轮新增 fsync 没有显示明显额外总成本；不能据此把其他 NFS 挂载推广为相同结果。

本地小文件成本显著增加符合逐文件同步的预期；SMB 1MiB 主机输出总吞吐约147MB/s。实际读带结果如下，不能只用 host benchmark 判断是否持续走带。

## 回归检查

完整 cargo test：189 passed、10 ignored；Clippy all-targets/all-features -D warnings 与正式 release 构建通过。新增6项回归覆盖最终文件 fsync 失败不发布完成、实际错误优先于断队列、多 extent 只在末端同步、空文件/纯稀疏文件、目录同步失败阻止成功。测试不依赖真实磁带；需要设备的性能测试保持 ignored。

计时字段 `file_sync` 仅表示文件 sync_all；本次 `syncfs` probe 包括目录/根目录 fsync 和末尾 syncfs，不能解读为单个 syncfs 系统调用耗时。原始测量见 [JSON](RESTORE_SYNC_FIX_2026-10-24.json)。

## 真实磁带 → SMB

读取原有 generation12 夹具 `/restore-research-20261024/size-1048576`，8GiB、8192个1MiB文件、16384次READ、一次数据LOCATE；磁带只读，未修改内容。8192个文件全部SHA匹配，测试输出在验证后删除。

- 读带跨度64.217秒，**133.77MB/s**；此前同一夹具本地恢复基线为133.84MB/s。这里比较完整8GiB跨度，不混用旧报告中间7GiB的132.17MB/s SMB窗口。此次只有一轮SMB实测，不能作细微差异的显著性判断。
- 计划34.489秒，恢复204.225秒；恢复包括18.766秒prepare、119.465秒首次数据LOCATE。计划耗时独立于恢复计时，不应与同步开销混淆。
- file_sync共8192次，累计42.236秒，中位5.100ms、P99 5.666ms；这是并行writer内的累计等待，与读带重叠，不能直接加到读带跨度上。
- SMB FLUSH从4049增至12241，增量8192，失败增量0。文件关闭累计0.036秒；旧版本的部分关闭等待现在发生在明确的fsync阶段，不宜单独比较close列宣称性能提升。
- 0.25秒采样的最大队列占用1.5MiB；发送队列累计0.024秒、获取buffer累计0.007秒，未显示持续目标背压。采样最大值并非瞬时绝对峰值，也未测量物理倒带次数。
- 仍有一次4.304秒SG_IO READ（sequence3351），与之前同物理位置的长READ现象一致；本次未定位其设备内部原因，不能归因为新增文件同步。

最终health查询：读/写uncorrected均0，无活动TapeAlert；其他纠错累计字段并非全零，完整文本保留在JSON。

结论：修复补齐了明确缺失的同步请求和失败传播。本轮NVMe共享上的1MiB真实恢复没有可辨识的持续读带吞吐下降；KB级文件的同步成本明确增加，机械盘、更慢服务器和其他挂载仍需另测。不以省略fsync换取原来的弱同步吞吐。

## 部署

正式release已替换 .125 的 `/home/ain/tapecpy-perf-20261023/bin/tapecpy`，`~/.local/bin/tapecpy`仍指向该位置，帮助命令运行正常。旧版保留为同目录 `tapecpy.before-sync-fix-20261024`。新SHA256为 `3880f32c2e39c8452653fd9e5b41075406e7bad520ba198c9cf3530e8cfdb85a`，旧版为 `c95cca714d807a112c5a371311b948e1ede639aaf53226c148723d4d1d0b5390`。

原始运行文件位于 .125 `/home/ain/tapecpy-sync-fix-20261024`，测试二进制before/after及SHA随报告JSON保留，脚本为 `scripts/network_test/test-restore-sync.py`；before/after为测试构建，正式部署使用独立的无测试release构建。
