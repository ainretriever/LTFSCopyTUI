# 顺序读写性能审计与验收

本文件按研究阶段保留历史记录。当前实现、已完成修复及剩余问题以
[2026-10-24 汇总](PERFORMANCE_SUMMARY_2026-10-24.md)为入口；各日期报告中的
旧二进制、同步策略和测量结果均按当时状态解释。


## 目标与环境

目标环境：LTO-5、LSI SAS2308，source/destination 为 Linux 已挂载 NFS 或 SMB，
文件从 KB 到 TB。目标是在 host 的持续吞吐和元数据处理能力足够时，减少应用自身
插入的等待。当前没有连接该硬件的实测结果，软件测试不能证明磁带不会 backhitch。

磁带只有一个 owner，所有 READ/WRITE/LOCATE/diagnostic 命令仍串行发送。
网络文件工作线程不得打开磁带设备。索引提交、VCI、设备 lease 和恢复文件的
create-new/syncfs 完成条件保留。

## 本轮已修复

* 普通 job 进度仍按 250 ms 节流，但 JSON 序列化和 fsync 由专用 worker 执行。
  worker 在写盘前复制状态并释放状态 mutex；磁带线程仅合并内存状态和发送非阻塞
  wakeup。wake 队列容量为 1，慢盘期间合并 checkpoint，不积累事件副本。
  telemetry/history 先应用到内存，不会因合并 wakeup 丢失 session worst。
* 阶段、取消、失败和终态仍同步保存。所有磁盘保存共用 persistence mutex，worker
  取得锁后才抓取最新快照，避免旧进度覆盖新终态。离开 operation（包括错误路径）
  前 join worker。checkpoint 失败请求安全取消，并在正常收尾时返回持久化错误。
* LTFS/RAW source reader 保留 EOF 探测使用的 allocation，并跨文件复用。完整块
  回收时保留已初始化长度，下一次读取无需把整个块先重新清零。短读仍合并，只有
  文件末 record 允许不足一个块，SHA-256 仍针对实际 payload。
* LTFS destination 回收块也保留长度。RAW 恢复和 TAR 写入改用有总 allocation
  上限的回收池，预算覆盖队列、producer 和 consumer 在途块。
* RAW 恢复将 SHA-256 放在 destination writer，与下一次 tape READ 重叠。
  只有全部数据排空、flush/sync 成功并取得最终摘要后才返回成功。
* 写入开始前一次遍历索引，解析本批次新文件的可变引用。FileEnd 按路径哈希表取出
  对应条目，避免每个小文件结束时再线性扫描兄弟条目。回读校验复用同一批量解析。
  索引树结构在这些引用存活期间不能改变，由 Rust 借用规则保证。
* 写后 SHA-256 校验跨文件保留已知的 partition/next block，只在不连续时 LOCATE；
  空文件不改变磁带位置。跨文件复用 READ allocation。普通恢复已有相同的连续定位
  优化，此次补齐的是校验路径。

普通运行进度现在是可提前通过 IPC 观察的内存状态；磁盘 checkpoint 可能滞后，
且慢盘时中间 checkpoint 会合并。阶段和最终结果仍经过同步 barrier。
这不提供断电续传承诺，主机/runner 崩溃后仍按原流程诊断，不自动重放写入。

## 当前限制（随后续实测更新）

1. LTFS source 仍是一个顺序 reader。KB 文件的 NFS OPEN/READ/CLOSE 往返可能成为
   瓶颈，即使网络带宽足够。destination 同样由一个 writer 消费文件边界。
2. 源缓冲已按实际片段容量分配，恢复已采用有界payload批次，解决此前KB文件浪费
   完整槽位和控制消息挤占容量的问题。512MiB仍不是进程RSS上限；描述符、工作缓冲、
   计划和索引另占内存，极小payload、频繁定位或描述符上限仍可能降低有效容量。
   不可直接增加无界线程或让多个线程并发操作磁带。
3. 写入保持每个文件独立的末 record。把多个小文件塞进同一磁带 record 涉及 LTFS
   extent byteoffset。当前已支持保存、读取和校验非零偏移，但尚未实现打包写入。
4. BER 仍每 5 秒在唯一设备线程同步查询。驱动器/HBA 若因诊断命令干扰 streaming，
   需要实测命令延迟和最低可用采样频率；不能另开设备线程并发查询。
5. stdout 单文件读取和 SHA-256 回读校验仍在设备线程消费/计算 payload。连续定位
   与 allocation 已改善，但尚未为这两条路径增加独立 destination/hash 管线。
6. 当前 release 配置没有显式 CPU 指令调优。不能未测 CPU 就把 hash 视为免费，
   也不能为提高速度而静默关闭 SHA-256。

## 可重复的软件验证

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked benchmark_large_directory_metadata_updates -- --ignored --nocapture
```

最后一项只测 20,000 个同目录文件的索引条目更新；包含批量查表的构建成本。
它不读取网络文件，不访问磁带，不能当成磁带吞吐提升比例。

本轮本机 debug 构建的一次结果：逐文件线性查找 1.365 s，批量解析加更新 19.1 ms。
这只是上述主机元数据微基准；编译模式、CPU、文件数和目录深度都会影响结果。

新增回归验证包括：单槽缓冲跨 100 个空/小文件只分配一次且摘要/边界正确；
回收池保持 allocation 和已初始化数据；状态盘被阻塞时 1,000 次进度更新仍完成；
最后 checkpoint 不回退已保存终态；持久化失败触发安全取消并返回错误。

## LTO-5 / NFS / SMB 真机验收

对每种挂载分别使用：单个大文件、足够长的中大文件混合目录、KB 小文件目录，
以及真实 KB 到 TB 混合目录。先测 host-only 顺序吞吐和实际元数据操作速率，
再分别执行正常写入、恢复和写后回读校验。正式运行使用 release executable。

记录 source/tape/destination 的区间吞吐、buffer payload/预算、等待状态、
CPU/单核占用、RSS、诊断命令和 checkpoint 延迟、驱动器错误计数与可用的
backhitch/stop-start 计数。相同数据分别比较旧版和新版，区分启动、稳态、
文件切换、分区切换、最终索引和同步阶段；最终平均速度不能替代稳态样本。

注入短时网络延迟和慢状态盘：确认容量足够时数据管线吸收波动；持续目标不足时
正确反压；取消在 record/事务安全边界收敛。恢复后比较完整文件列表、长度和
SHA-256，并独立检查两个 index/VCI generation 一致。没有真机 backhitch 计数和
长时间数据完整性验证之前，不宣称“最大性能”或“绝不暂停”。

## 参考实现核对

按仓库开发约定检查了 LTFSCopyGUI 的
[IOManager](https://github.com/LCG-Dev-Group/LTFSCopyGUI/blob/main/LTFSCopyGUI/Core/IOManager.vb)、
[LTFSWriter](https://github.com/LCG-Dev-Group/LTFSCopyGUI/blob/main/LTFSCopyGUI/Forms/LTFSWriter.vb)
以及 Rust fast reader 的 hash/small_files 实现。上游包含 buffer pool、异步 hash 和
专门的小文件调度；这些是实现选择，不是 LTFS/SCSI 规范要求。本轮独立实现 Linux
allocation 回收和 job checkpoint worker，不复制上游结构或源码。磁带数据命令仍为
逐 record READ/WRITE，连续区间直接前进；阶段切换的 filemark、partition、index 和
VCI 命令序列没有因 host 并发改变。

## P15 / P18 / P12 后续修复

* P15：索引往返、持久化读计划、普通恢复/stdout/verify 均支持非零 byteoffset；
  物理调度增加 byteoffset 排序，最多缓存一个共享 record。部分块切片会复制；
  完整消费的整块仍直接移动。旧 job 缺失偏移按零处理。仍未增加打包 writer。
* P18：label 块大小在解析时校验为本实现支持的 1..=1 MiB；extent 偏移和加法
  溢出在读取前拒绝。READ/WRITE(6) 超过 0xffffff bytes 在提交前返回错误。
* P12：完整计划只保存在 spec.json，普通 state 和 IPC 省略 preflight；进程内
  计划通过 Arc 共享。runner 从完整 spec 恢复计划；旧状态兼容，旧 server 超限
  返回可回退磁盘；TUI 对未改变的终态任务复用缓存。

新增验证：非零偏移 index round-trip、共享 record/跨块/跨分区 SHA-256、实际短
record 越界拒绝、2 万文件计划的摘要与恢复、旧偏移字段兼容、终态缓存失效，以及
块大小/24-bit CDB 长度边界。

生成实机测试数据：

```bash
python3 scripts/prepare_perf_data.py /磁盘目录/source --large-gib 32
```

脚本要求全新目录，写入 os.urandom 内容而非稀疏文件，同时生成独立 SHA-256 清单。
不要用 tmpfs 作为持久磁盘吞吐基线。localhost NFS 可用于验证网络文件系统调用和
协议路径，但没有真实网络链路/远端存储竞争，不能替代远端 NFS/SMB 性能验收。
非零 byteoffset 的真机互操作还需要独立实现生成的 packed 卷；tapecpy 自写自读
只能覆盖当前零偏移 writer，不能充当该项互操作证据。

## 2026-10-23 换带对照遥测

通道遥测在既有 5 秒查询结果中保留原始 before/after 计数（C1、C1 uncorrectable、header、write-pass、CCP），连同 diagnostic page code 和该次采样的 partition/logical block，写入 job progress 的 `channel_counter_window`。字段可缺省以兼容旧状态；状态仅保留最近一个窗口，测试监视脚本持续保存到 JSONL，不增加设备命令，也不在状态内堆积完整原始历史。程序全局 worst-rate 仍沿用既有占位规则，分析时必须排除 `ccp_advanced=false` 的 -2.98。

## 2026-10-23 网络测试发现

已修复 autofs 包装遮蔽 nfs4/cifs 真实挂载身份：网络优先排序后用全局 mount-point 集合去重，替代仅相邻去重；回归测试覆盖 interleaved 的两种网络挂载。新作业重新冻结实际后端来源。

已于 2026-10-24 修复：此前写入容量预检直接使用物理分区 0 的 MAM remaining，格式化卷上该分区为 index，导致约 35.6GiB 的容量被误认为数据分区容量。现在按有效 LTFS label 的 data_partition 显式查询，CLI/TUI/runner 一致处理，失败不回退到 index 容量。2026-10-23 网络测试当时用只读独立探针确认 data 分区 1 剩余 1,352,451MiB 后确认 warning，历史证据保留。

NFS 大文件写入保持约 144MB/s、源缓冲充足；约 39MiB 的小文件尾段明显较慢。此为需要进一步拆分 NAS 元数据往返与逐文件磁带命令开销的观测，不能把大文件结果推广为所有文件大小均可持续满速。

## 2026-10-23 任务内温度

在已有 5 秒 BER 采样边界接入温度 LOG SENSE 0Dh，由唯一设备 owner 查询，随
job progress 发布最近一次尝试（含时间、温度及错误）。独立处理温度/BER 失败；
无 BER baseline 不再跳过整次遥测。TUI 活动任务优先使用 job 温度，pending/N/A/
超过 15 秒 stale 均显式显示。旧任务缺字段兼容。

验证：完整 cargo test 173 passed、2 ignored；Clippy all-targets/all-features -D warnings
通过。新增测试覆盖采样节流、BER 失败时仍发布温度尝试、状态序列化和旧状态兼容，
以及 pending/有效/失败/过期显示。随后 Quantum LTO-5 短时 A/B 实测未发现明显持续
吞吐下降，温度查询平均 2.714ms，每 5 秒周期约占 0.054% 的设备 owner 时间。
正式实现默认启用；测试开关和计时仅存在于独立测试副本。详见
[TEMPERATURE_SAMPLING_TEST_2026-10-23.md](TEMPERATURE_SAMPLING_TEST_2026-10-23.md)。
测试机常用主程序尚未替换，后续部署应使用正式 release 构建。


## 2026-10-24 容量修复验证

CLI、TUI 和 runner 统一使用 VolumeInfo 中显式查询的 LTFS data_partition 容量；
查询失败不回退到 p0。回归覆盖 data=0/1、失败、坏长度、零余量和错误分区角色。
Quantum PERF02 装载后 index=36,476MiB、data=1,279,001MiB，100GiB 预检 Normal；
未装载时 Unknown。修复已部署 192.168.10.125，旧程序备份保留。

## 2026-10-24 小文件分层实测

完成108项真实主机方法/源管线测量、54项受限并发探针和576MiB磁带record夹具。
4KiB平铺源管线中位数：local 201.37、NFS 18.56、SMB 6.16MB/s；目标数据阶段
NFS 4.47、SMB 3.54MB/s，另有显著create准备成本。均为现有缓存配置，非服务器冷盘。
4KiB SG_IO命令中位write 65.64us、read 155.81us，说明主机逐文件开销和同步
小record成本是两个独立限制。全部52,608磁带文件SHA匹配，generation 9 Healthy。
尚未据此改变正式小文件管线或record布局；详细限制、原始数据和下一步见
[SMALL_FILE_RESULTS_2026-10-24.md](SMALL_FILE_RESULTS_2026-10-24.md)。

## 2026-10-24 源读取与小文件缓冲优化

已完成路径查询优化和按需缓冲分配，未增加并发或调整网络缓存。144项主机对照、6项预填充和两次真实磁带写入/完整SHA回读完成。详见 [源读取优化报告](SOURCE_OPTIMIZATION_RESULTS_2026-10-24.md)。

## 2026-10-24 不变源文件校验开销实验

只在测试构建中跳过源校验，72项四轮主机测量及4项系统调用探针均通过SHA校验。4KiB NFS收益不足1%、SMB无稳定收益；64KiB结果受运行顺序/缓存显著影响，暂不加入正式选项。详见 [实验报告](STABLE_SOURCE_EXPERIMENT_2026-10-24.md)。

## 2026-10-24 恢复方向四项研究

完成真实管线MB级输出、句柄/有效缓冲、分阶段观测及三目标32GiB实机恢复。local/NFS/SMB读带跨度分别138.74/138.75/138.72MB/s，全部SHA匹配；最终generation12 Healthy。新确认有效缓冲过早受限、等待漏报、SMB目录规模准备成本及完成同步语义问题，未实施算法优化或默认并发。详见 [完整报告](RESTORE_PERFORMANCE_RESULTS_2026-10-24.md) 和 [原始数据](RESTORE_PERFORMANCE_RESULTS_2026-10-24.json)。

## 2026-10-24 恢复同步语义修复

恢复文件最后一个 extent 在打开句柄上 sync_all 并检查 close 后才计为完成；空/纯稀疏文件同样处理，结束时同步目录与根目录并保留 syncfs。同步失败禁止 Completed，真实 writer 错误不再被队列断开掩盖。SMB 配对测试确认逐文件 FLUSH 已发出；成本及实机验证见 [同步修复报告](RESTORE_SYNC_FIX_2026-10-24.md)。此前恢复性能报告保留为旧同步策略的历史基线。

## 2026-10-24 恢复有效缓冲与等待指标

恢复目标管线改用有界1MiB主机payload批次，文件/extent控制消息与数据范围随批次保序传递，不改变磁带record或逐文件同步。默认512MiB容量探针中4KiB–4MiB文件模式均积累满512MiB payload，额外描述符与工作缓冲内存有界、另计。等待回收块时由原设备owner定期发布状态和检查取消，累计次数/时长写入job并在TUI显示，持续停读时继续更新吞吐采样。详见 [缓冲与指标报告](RESTORE_BUFFER_FIX_2026-10-24.md)。
