# 小文件性能调查：第一轮（2026-10-23）

## 已确认的大文件基线

现有 Quantum LTO-5 / 2.5GbE 客户端 / 10GbE NAS 配置下，1–31GiB 稳定区间
NFS 写/读中位数 144.448/144.422MB/s，SMB 写/读 144.439/144.390MB/s。
写入保存的样本中源缓冲始终至少 90% 满，恢复样本未观察到目标缓冲堆满。
这是当前配置的持续吞吐结论，不代表所有配置无瓶颈或没有短暂停带。

## 只读主机基线

脚本 `scripts/network_test/small-file-baseline.py` 在 tapeserver 上读取既有 10000 个
4KiB 文件，比较本地/NFS/SMB，包含打开、读取、SHA-256、关闭；不访问磁带。
每种路径依次 1/4/16/1 workers，未清理客户端/服务端缓存，全部内容与本地相符。
Python 和线程调度开销包含在内，不是 Rust pipeline 本身的测量。

| 路径 | 首次串行 MB/s | 4 workers | 16 workers | 再次串行 MB/s |
| --- | ---: | ---: | ---: | ---: |
| 本地 | 112.946 | 141.626 | 138.451 | 629.297 |
| NFS | 10.029 | 70.224 | 88.572 | 21.149 |
| SMB | 6.623 | 11.490 | 14.547 | 6.363 |

枚举/stat 10000 文件分别 0.052/1.187/0.242 秒；不计入表中读取时间。
这是固定顺序的探索性测试，缓存影响明显，不能据此把差值全部归因于并发，更不能
用此表宣布协议优劣。重复串行 SMB 仍约 1553files/s，远低于 4KiB 文件要达到
144MB/s 所需的约 35156files/s，说明带宽充足不等于逐文件数据路径足够快。
原始数据见 `SMALL_FILE_BASELINE_2026-10-23.jsonl`。

## 当前实现的独立限制

1. `produce_source_blocks` 单线程逐文件 open/read/hash；网络请求等待不能通过源
   文件并行预取重叠。source 扫描另有逐文件 metadata/open，需与实际数据阶段分开测。
2. 写入一个 4KiB 文件产生一条独立 WRITE，文件尾块不与下一文件共享 record；
   每文件 extent 的 byteoffset 固定零。恢复该卷时也至少读取对应的小 record。
   没有逐文件 filemark，不能把损失归因于不存在的逐文件 filemark。
3. 512MiB source buffer 的 allocation/消息容量按 LTFS block 数量计算，而不是按
   小文件有效载荷计算。512KiB block 对应 1024 个 buffer；只装 4KiB 时 pool 的
   有效 payload 上限约 4MiB，消息槽还需容纳 FileStart/FileEnd，实际排队更少。
   恢复也按最大 record 大小限制 allocation/消息数。单纯增大名义 byte budget
   不能等同于已有足够的小文件预取窗口。
4. source 对小文件和 EOF probe 仍 resize 到完整 block，可能存在多余清零和内存
   写入；需要 CPU profiling 后确认贡献，不能只按读取的 4KiB 估计开销。
5. ReadOutputFiles 在准备阶段创建全部文件，最多保留 256 个 handle，后续重新打开
   其他文件；这些额外 create/close/open 在 NAS 上可能有明显延迟。恢复已经使用
   最终一次 syncfs，没有逐文件 fsync。不能以去掉不存在的 fsync 作为优化方案。
6. 任务状态普通进度已经节流并异步持久化，仍应统计 per-file 内存更新、锁和通知
   耗时，确认其对高 files/s 是否显著，而不是先假定设备线程每文件 fsync 状态。

## 下一轮实验与实现顺序

先测主机源和目标的独立开销，再测磁带 record 路径，最后组合：

- 控制顺序/缓存的多轮 1/4/16 worker host-only 对照；真实 SourcePipeline 接空 sink，
  分开报告 scan、open/read/hash、有效 payload 和 files/s。
- 用新建独立目录测实际 ReadOutputFiles prepare/reopen/write/syncfs，记录各阶段
  耗时与系统调用计数，保留 create-new、安全路径和最终同步语义。
- 用内存预备好的数据比较等量 4KiB 与 512KiB record，测 SCSI 命令延迟和命令数；
  与 NAS 脱钩，保持唯一设备 owner，观察是否有物理 backhitch 指标。
- 若源等待占主导，设计受 byte/任务数双预算约束的并发小文件预取，保持冻结计划
  顺序、hash、取消和错误传播；设备仍仅一个 writer。
- 若小 record 占主导，再设计 LTFS 跨文件 block packing。现有读取已支持非零
  byteoffset，但 writer 尚未生成共享 record；实施前需复核 LTFS 格式、索引、
  部分文件失败与回读校验，不作为一次简单的网络参数修改。

本轮仅新增调查脚本和记录，没有修改生产数据路径、网络挂载参数或磁带内容。
