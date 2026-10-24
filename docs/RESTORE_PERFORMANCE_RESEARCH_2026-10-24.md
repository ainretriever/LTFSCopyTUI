# 磁带→NAS 恢复性能研究计划

日期：2026-10-24。状态：计划中的四项研究已执行，见 [结果与分析](RESTORE_PERFORMANCE_RESULTS_2026-10-24.md)。以下保留原研究设计；未实施恢复算法优化。

## 目标与边界

重点解释并改善 MB 级文件恢复到 SMB/NFS 时的吞吐下降，同时覆盖 KB 级和大文件。不以更大缓存掩盖持续供需差距，不默认增加机械盘并发，不改变 LTFS record 布局。磁带保持单设备所有者；主机目标操作留在独立输出管线。分别报告准备、定位、数据、排空与同步时间，以及从开始到完成的总时间。

本轮参考此前 PROJECT、WRITE_WORKFLOW、ARCHITECTURE 设计及 SMALL_FILE_RESULTS_2026-10-24 实测。前次目标微基准没有经过真实 ReadDestinationPipeline，1MiB 文件总量仅256MiB，不能证明完整恢复或长时间稳态没有瓶颈。此前“目标性能足够”或“大文件正常”的结论不能扩展到所有 MB 级恢复场景。

## LTFSCopyGUI 源码审查

本次实际获取并检查的上游为 https://github.com/LCG-Dev-Group/LTFSCopyGUI ，旧 zhaoyangwx 地址重定向至此。固定提交 `08fa2ec8b00e93630a9b89669d2dc8d3e8de37dd`（2026-10-16）。参考源码留在被忽略的 references/LTFSCopyGUI，仅研究，不复制或分发。不能确认用户曾使用版本与本提交相同，也没有在 Windows 上复现，因此以下为代码事实及性能假设，不是对既往故障的定案。

源码链接：[LTFSWriter.vb](https://github.com/LCG-Dev-Group/LTFSCopyGUI/blob/08fa2ec8b00e93630a9b89669d2dc8d3e8de37dd/LTFSCopyGUI/Forms/LTFSWriter.vb)。

- `RestoreFile`（6089起）保留传统路径：检查目标存在、大小/时间，必要时 WriteAllBytes 建空文件，然后重新 FileStream 打开；磁带 ReadBlock 与 fs.Write 在同一循环。目标写入和文件收尾延迟会推迟下次磁带读取。6143 的8MiB FileStream缓冲不等于跨文件磁带预读管线。
- `PrepareExtractionFile`（约5620–5710）新提取路径：检查目标/临时文件、创建临时文件、再次打开以处理 sparse 和 SetLength；这些有恢复完整性/续传用途，不能简单删去。
- `RestoreExtractionExtent`（5831起）：每个 extent 打开临时文件、检查长度、Seek、循环 ReadExtractionBlock→output.Write，结束 Flush/关闭。这里仍是同步交替，未在该路径看到跨文件读带/写NAS双线程队列。一个文件多个 extent 会增加重复打开次数。
- 当前新路径已经把 Seek 放在 extent 开始处，并明确注释“逐 block Seek 会 flush 写缓冲，破坏 SMB 批处理”（5853附近）。这是一条应主动防止回归的经验，不能声称该缺陷在当前新路径仍存在。
- `CommitExtractionFile`（5905起）执行存在检查、替换/重命名和属性收尾；`ApplyExtractionAttributes` 分别设置多个时间/属性。它们也在下一 extent 前串行执行，可能造成跨文件空隙。
- `BuildExtractionPlan`（5995起）已经按物理 partition/block/byteoffset 排序；`EnsureExtractionPosition`（5805起）仅在当前位置不匹配时 LOCATE。不能指控当前新路径无条件每文件定位。
- IOManager.vb:681 的 WriteThrough 位于 hash/copy 流程，不能拿来证明上述直接磁带恢复默认使用 WriteThrough；普通 Flush() 也不能不经协议观测就等同一次 SMB 持久化 FLUSH。

结论：借鉴其操作和教训，重点防止“网络文件收尾挡住读带”“逐 record Seek/Flush”“大缓冲但没有跨文件流水线”；不沿用 WinForms 线程结构，不混淆不同恢复入口。

## tapecpy 当前代码与优先假设

位置均在 src/app/mod.rs，后续修改以函数名为准。

|观察|已知事实|待验证的问题|
|---|---|---|
|ReadOutputFiles::prepare|先建全部目标，仅保留256个句柄，其余在open_extent重开；普通连续文件不强制set_len|创建/重开占总时间多少？256阈值是否出现台阶？提前完成与真正减少开销有何区别？|
|ReadDestinationPipeline::spawn|512MiB按READ_BUF_LEN=1MiB换算为512个buffer，消息槽514个；extent起止也占消息槽|小record和小文件是否在实际有效payload远低于512MiB时背压？混合extent下如何变化？|
|输出线程|独立线程，逐extent打开/Seek一次、write_all、finish；不是逐record Seek|写系统调用、close、重开、extent边界分别贡献多少延迟？|
|sync_filesystem|完成时syncfs，无逐文件fsync|最终排空有多长？共享挂载上其他写入是否混入？错误能否准确传回？|
|ExtentReadCursor|保持位置，连续extent不重复LOCATE，缓存共享record|真实命令数是否符合计划？不连续选择、共享record、跨分区是否引入必要或多余定位？|

接收record时必须容纳合法最大record，不能照搬源小文件策略，把SCSI读缓冲缩成剩余文件长度。研究可使用完整record接收缓冲与按实际payload计费的队列分离，但要计入暂存、复制、共享record和回收内存。

## 阶段一：先补观测，不先改算法

在测试构建增加分阶段累计耗时和计数，汇总不逐record打印：

- 目标验证、mkdir、create、set_len、重新open、Seek、write、close/drop、最终syncfs；次数及总耗时，关键操作P50/P95/P99/max。
- Tape LOCATE/READ命令数、耗时、相邻READ之间主机空隙；区分命令自身慢、目标队列背压、观测/调度成本。保持设备所有者内采样。
- 队列有效字节、record数、控制消息数、实际分配容量、峰值RSS；累计及最长背压等待，目标空等时间；按时间记录磁带读取和目标写入累计字节。
- 当前温度、完整通道计数/错误率窗口、TapeAlert。主机等待不能直接称为物理停带；若无可靠backhitch计数，仅报告可观测READ间隙及等待原因。
- 独立短探针获取strace和NFS mountstats / CIFS统计差值。区分用户态open/close与线上OPEN/CREATE/CLOSE/FLUSH；统计是挂载/系统级时隔离其他负载。服务器观察CPU、磁盘队列和延迟、网络，标明NVMe或HDD。

## 阶段二：三层对照，先定位 MB 级拐点

|层次|测试|用途|
|---|---|---|
|A 主机输出|确定性内存源→真实ReadDestinationPipeline→本地/NFS/SMB|隔离目标端，补齐旧微基准未覆盖的队列和extent事件|
|B 磁带基础|同一组extent→空接收端（仍校验）以及本地SSD|分清record命令限制、必要定位与主机输出成本|
|C 端到端|相同磁带extent→本地/NFS/SMB，通过正常恢复作业|验证源/目标交互、持续背压、最终同步和总耗时|

先跑1MiB、4MiB、16MiB、64MiB，重点捕捉用户报告的MB级问题；再补4KiB、64KiB及混合大小，0字节作正确性边界。平铺/分目录分别测。1MiB重点测255/256/257/1024文件，判定句柄缓存阈值，1024文件先作短筛选；稳态阶段每个关键组合使用8–16GiB或更大不可压缩数据，按实测确认缓存填满后仍有至少60秒有效观测窗口。若服务器内存吸收整组写入，应延长数据集并观察最终同步/服务端磁盘，不把内存缓存速度当落盘速度。

固定文件大小改变record/extent布局，固定record大小改变文件边界：区分“文件只有1MiB”和“record很小”这两种成本。已有磁带没有1/4/16/64MiB连续夹具时先记录缺口，再准备独立追加目录；不重写现有数据。现有小record夹具保留，读取优化无法消除已记录的小record所需READ次数。

每一层先筛选，只有发现问题的组合进入4轮AB/BA/AB/BA或同等平衡重复；当前正式二进制为基线，每项新建独立目标目录、禁止覆盖，不在同一后端上同时跑NFS/SMB。缓存状态显式标注为热、重新验证或受控冷，不通过调整挂载参数制造不对等对照。不默认drop全机缓存；不明缓存状态即保留限制说明。

连续物理选择与刻意分散选择分开。已知不连续extent的定位耗时单列，不能拿碎片化任务推导连续恢复性能。

## 阶段三：按证据做单变量优化

1. 优先解决恢复队列的有效容量：字节预算与消息上限分开，减少控制消息挤占，测大/小/混合record，不增加设备并发。用固定速率伪磁带和可控延迟目标测缓冲满后背压及取消，真实磁带再确认。
2. 若create→close→reopen显著，研究有限窗口提前创建并保留句柄到首次extent，及重用安全父目录句柄。保持create-new、不覆盖和防符号链接约束。把“全任务提前失败”变成“窗口内发现失败”会改变语义，必须先设计错误、残留文件、磁带停止点，再实施。不能为速度无限保留句柄。
3. 若WRITE粒度太碎，测试同文件内有界合并/批量输出；保持extent偏移、洞、共享record和文件SHA正确，不跨文件混写。不添加逐record Seek/Flush，不删最终同步。
4. 只有单输出线程确认为协议延迟瓶颈，才测试1/2/4个目标worker；按文件保持写入归属、全局内存/句柄上限与最终错误汇总。HDD不默认并发，单线程保留基线；无真实HDD就不宣布适用于机械盘。
5. 挂载缓存、NAS端代理、本地SSD暂存均单独立项，待低成本本机管线优化的端到端收益明确后再讨论。

## 接受标准与交付

- 每项实验记录二进制SHA、提交/工作树差异、磁带generation、文件/extent/record布局、挂载参数和服务器存储类型。
- 重点报告1/4/16MiB持续有效吞吐、files/s、总耗时、准备/同步耗时、P99文件边界空隙、读带背压时间、实际队列占用；不能只展示短时峰值或扩大缓存后的初始速度。
- 优化需重复改善真实恢复总时间或显著减少可解释的主机背压，并且无明显大文件回退、无无限内存/句柄增长；约5%以内波动先视为不确定并复测。没有提升就不合入复杂设计。
- 独立重读目标逐文件验证长度、SHA；共享record、多extent、乱序文件偏移、稀疏文件、空文件、同名冲突、路径替换、权限错误、ENOSPC、网络故障和取消作有针对性的正确性回归。同步失败不可报告完成；syncfs成功只表示现有OS/服务器承诺，不能证明断电硬件持久性。
- 取消/失败后设备与线程可回收，下一任务可启动，TUI无阻塞；磁带最终状态可诊断。读带顺序与设备单所有者不受主机目标并发影响。

建议第一批交付：阶段一观测 + A层1/4/16/64MiB筛选 + 256句柄阈值和队列预填充探针。根据这批结果只选一个最有证据的优化，再进行B/C层实机AB对照。当前计划不预设并发是答案。
