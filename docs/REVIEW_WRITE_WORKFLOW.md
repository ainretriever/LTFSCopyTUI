# 写入工作流审阅记录（2026-08-09）

本文记录对 tapecpy 写入路径与 LTFSCopyGUI 参考实现对照审阅的结论，
以及据此确定的改造方向。供后续实现与架构调整引用。

## 1. 背景：写入失败与定位到的根因

在专用测试磁带上验证 `tapecpy write` 时，写入位置反复错位：

- 首个文件记录为 start_block=7（按 index 计算的位置），但数据没有落到该处；
- 后续写入甚至覆盖了分区头部（VOL1 label）；
- 读回时 index 完好、但 extent 指向的位置没有数据（"幽灵 extent"）。

实测驱动器行为（Quantum Ultrium 5）：

- **mkltfs 格式化后，驱动器的 EOD 标记与磁带实际内容不一致**：
  data 分区实际内容到块 7（label 0-4、初始 index 5、FM 6），
  但 SPACE-to-EOD 报告 **0**；
- 该驱动器**拒绝写入到其 EOD 标记之后**（WRITE 返回 BLANK CHECK /
  END OF DATA，ASC 0x00/0x05），所以写入计算位置 7 直接失败；
- 在驱动器 EOD 处（块 0）写入则成功，且之后 EOD 跟踪恢复正常
  （再次 SPACE-to-EOD 返回正确位置）。

结论：**不能依赖驱动器的 EOD 标记来确定追加位置**。
LTFSCopyGUI 正是这样做的 —— 它完全不使用驱动器 EOD 定位。

## 2. LTFSCopyGUI 的写入机制（参考实现）

### 2.1 写入定位（`LocateToWritePosition`，LTFSWriter.vb:4043）

不依赖驱动器 EOD，而是从磁带内容推导：

1. LTFS 约定 **data 分区始终以「最新 index 文件 + filemark」结尾**；
2. 每个 index 的 `previousgenerationlocation` 指向 data 分区中
   上一个（最新的）index 文件；
3. 写入新数据前：`Locate(previousgenerationlocation.startblock, DataPartition, Block)`
   → `ReadToFileMark()` 读完该 index 文件 → 当前位置即数据追加起点；
4. `CurrentHeight`（软件跟踪的 data 分区高度）作为后续定位的备份。

### 2.2 数据写入

- `SetBlockSize(plabel.blocksize)` 设置驱动器块大小；
- 实际写记录用 WRITE(6)（cdb[1]=0，可变块语义，与 OpenLTFS 相同）；
- 软件维护 `p.BlockNumber`，文件 `extent.startblock = p.BlockNumber`。

### 2.3 index 更新（`WriteCurrentIndex`，LTFSWriter.vb:2611）

index **先写到 data 分区**：

```text
Locate(0, DataPartition, EOD)
→ WriteFileMark
→ Write(index XML)
→ WriteFileMark
```

`schema.location` = data 分区中新 index 的位置；
`previousgenerationlocation` = 旧 index 位置；generation + 1。

### 2.4 index 分区同步（`RefreshIndexPartition`，LTFSWriter.vb:2710）

同一份 index 镜像到 index 分区：

```text
Locate(3, IndexPartition, FileMark)   // label 之后的第 3 个 filemark
→ WriteFileMark → Write(index) → WriteFileMark
```

同步后 `location` 指向 index 分区副本，`previousgenerationlocation`
保持指向 data 分区最新 index。

## 3. tapecpy 代码的问题（对照结论）

1. **`write_file` 从不维护 data 分区 index**：只写 index 分区。
   这违反 LTFS 结构，也使我们无法采用 LTFSCopyGUI 的定位方法，
   被迫依赖驱动器 EOD（而它不可靠）。
2. **`data_append` 计算错误**：从 extent 求 max(末尾 + 1 FM)，
   漏掉了 data 分区末尾的 index 文件及其 filemark。
3. **依赖 `space_to_eod`**：mkltfs 后驱动器 EOD 错乱，
   且驱动器拒绝写入其 EOD 之后。
4. **index 写入缺前置 filemark**：LTFS index 文件由
   `[FM][index][FM]` 界定，参考实现均先写 FM。
5. **扫描终止条件**依赖驱动器 EOD（已退化为读到 blank check，正确但慢）。
6. 次要：`set_variable_block` 从 MODE SENSE(6) 字节 4 取密度，
   前提是块描述符存在，需要核实；
   WRITE(6) cdb[1]=0 与两个参考实现一致，没有问题。

## 4. 确定的改造方向

1. **定位**：读 data 分区最后一个 index 文件（沿
   `previousgenerationlocation` 链；mkltfs 的初始 index 的
   previousgenerationlocation 指向 data 分区块 5），
   定位到其 filemark 之后作为数据写入起点；软件跟踪位置。
2. **数据写**：可变块 WRITE(6) 逐块写。
3. **index 更新**：写 FM → 新 index 写到 **data 分区**
   （location 更新、previousgenerationlocation 指向旧 data index）
   → 同一份镜像到 **index 分区**。
4. **不再把 `space_to_eod` 当作定位依据**（可留作校验/优化提示）。
5. VCI（MAM volume coherency）与 PEWS 留待后续，暂不实现。

## 5. 待验证问题

- OpenLTFS 挂载 mkltfs 格式化磁带时同样调用 `tape_seek_eod`（驱动器 EOD），
  如果它同样踩到 EOD=0 的坑，说明这是 OpenLTFS mkltfs 的已知缺陷，
  我们的实现更应完全绕开驱动器 EOD。
- 驱动器在 mkltfs 后处于的块模式，以及是否有一种模式组合能让
  驱动器 EOD 报告正确（关系到能否保留 `space_to_eod` 作为快速路径）。

## 6. 真实设备复验（2026-08-09）

测试设备：Quantum ULTRIUM 5，固件 3210，`/dev/sg1`；测试介质可任意覆盖。

重新用 OpenLTFS 2.4.8.4 格式化后，初始布局为：

```text
index generation 1: a:5
data  generation 1: b:5，结束 filemark 后追加位置 b:7
```

复验确认了以下实现要求：

1. 普通数据文件结束后不能先单独写 filemark、再为 index 写第二个
   filemark；数据区与 index 之间只应有一个边界 filemark。
2. index 分区刷新不是在旧 index 的尾部继续追加，而是从最新 index
   前的 filemark（本例 `a:4`）覆盖，因此新 index 仍从 `a:5` 开始。
3. SG_IO 原实现对所有命令使用 10 秒超时。真实设备上的 WRITE(6) 和
   WRITE FILEMARK 均超过 10 秒，被 Linux SCSI 层 task abort。磁带运动、
   数据读写和落带命令需要长超时；当前采用 1800 秒上限。
4. 不能只检查 SCSI status；host status 或 driver status 非零同样表示
   命令失败，否则内核 task abort 可能被误判为成功。
5. 后续故障注入测试确认 MODE SELECT(6) 也可能超过 10 秒并返回
   `host_status=0x0003`，因此设置磁带块模式的 MODE SELECT 同样使用
   1800 秒长超时。

修正后完成两次连续写入：

```text
generation 2:
  first.bin  1024 bytes，extent b:7
  data index b:9
  index copy a:5

generation 3:
  second.bin 614400 bytes（两个 records），extent b:11
  data index b:14
  index copy a:5
```

两个文件均由 tapecpy 读回并通过 SHA-256 比较。OpenLTFS 能以只读方式
挂载 generation 2 和 generation 3，识别 `(a,5) -> (b,9)`、
`(a,5) -> (b,14)` 的 index 链及两个文件，首个文件经 OpenLTFS 读取的
SHA-256 也一致。

后续边界测试还确认：

* 零字节文件可以写入、列出和读回，OpenLTFS 显示长度为 0；
* 文件名中的 `&`、`<`、`>` 能被 serializer 正确转义；
* 原 parser 会在 Quick-XML 的 `GeneralRef` 事件处截断名称，现已改为在元素
  结束前累计 Text/GeneralRef，并加入 XML entity round-trip 单元测试；
* 修正后 tapecpy 和 OpenLTFS 均能识别 `xml&less<tag>.bin`。

### 6.1 data/index 代际分叉故障注入

在 generation 5 上写入新文件，并在 data index `b:25` 及其尾部 filemark
完成后、刷新 index 分区前强制终止进程，成功制造：

```text
index partition: generation 5 @ a:5
data partition:  generation 6 @ b:25
```

逐块扫描确认新数据和 generation 6 index 均完整存在，但旧版 tapecpy 只读取
index 分区，因此仍显示 generation 5。OpenLTFS full medium consistency check
检测到分叉，日志明确报告 `Recover an index on IP from (b, 25)`，随后把
index 分区恢复为 generation 7；恢复文件经 tapecpy 读回的 SHA-256 一致。

据此增加写入前保护：读取 index 分区引用的 data index 后，继续扫描至物理
EOD。只要引用 index 的尾部 filemark 后还有任何数据、filemark 或更新的
data index，就拒绝写入，避免从旧追加位置覆盖尚未同步的数据。

第二次故障注入制造 generation 8 / generation 9 分叉后，新保护正确报告：

```text
data/index 分区不一致：检测到 generation 9 data index；拒绝写入以避免覆盖
```

拒绝后 index 分区仍为 generation 8，没有产生新的写入。

## 7. 当前仍未解决的问题

- tapecpy 尚未更新 MAM Volume Coherency Information。OpenLTFS 挂载时会
  因此执行 full medium consistency check，随后自行更新 MAM coherency data。
- data/index 代际分叉目前能够在写入前检测并拒绝，但 tapecpy 尚不能自行把
  data 分区的更新 index 恢复到 index 分区，仍需借助 OpenLTFS 或未来的
  repair workflow。
- 当前只验证了正常完成和写入前拒绝；WRITE、filemark、data index、index
  镜像各阶段的故障注入及恢复策略尚未建立。
- `set_variable_block` 从 MODE SENSE(6) block descriptor 读取密度的兼容性
  仍需针对无 block descriptor 的响应验证。

## 8. 批量目录写入复验（2026-08-09）

单文件写入稳定后，Application 层增加写入计划器与批量执行器：

1. 写入前递归扫描源目录，按文件名稳定排序；
2. 在内存 index 副本中预先创建目录/文件条目并分配 UID；
3. 在首次 WRITE 前完成父目录、目标冲突、UTF-8 文件名、文件类型和基本
   可读性检查；
4. 所有普通文件在 data partition 连续写入，文件之间不写 filemark；
5. 每个非空文件记录自己的起始 block 和 bytecount，空文件不创建 extent；
6. 整个批次结束后只写一次 data index，再镜像一次 index partition。

真实设备在已有 generation 10 卷上追加 `/batch-one`：

```text
3 directories
4 files
719181 bytes
generation 10 -> 11
highestfileuid 8 -> 15
```

测试树包括小文件、零字节文件、跨两个 LTFS records 的 700 KiB 文件、两级
嵌套目录和名称含 `&` 的文件。tapecpy 能浏览完整目录树，四个文件逐一读回的
SHA-256 均与源文件一致。OpenLTFS 成功挂载 generation 11，识别批量目录树，
日志确认 index 链为 `(a,5) -> (b,40)`，随后安全卸载。

当前批量范围有意保持有限：符号链接和特殊文件会在规划阶段拒绝；尚未实现
源文件内容 hash、文件变化的完整快照语义和取消。

## 9. WriteSession、结构化事件与 write-anywhere 复验（2026-08-09）

Application 层现在用 `WriteSession` 统一承载一次完整写入，并通过结构化
`WriteEvent` 报告：

```text
Preparing
WritingData
FinalizingDataIndex
SyncingIndexPartition
Completed
```

事件包含当前文件、完成/总文件数和完成/总字节数。CLI 只消费事件并转换为
文本，不再需要知道 partition、filemark 或 index 刷新顺序。兼容入口
`write_file` 保留为无 observer 的包装。

首次真实事件测试在 generation 11 卷上被外部 SSH 会话中断；data 分区的
generation 12 index 已完整写到 `b:46`，但 index 分区原 label 被覆盖，探针
发现块 0 已是 EOD，而新的 index 出现在其后。该结果证明此前文档中记录的
write-anywhere 风险不是理论问题：OpenLTFS 卸载后，驱动器状态不能依赖。

实现因此增加 MODE SENSE/SELECT(10)，写入前检查 device configuration
extension page `0x10/0x01`。若 append-only 已启用，则按 OpenLTFS 的顺序先
无弹出卸载、清除 append-only、重新装载并恢复可变块模式；任何一步失败都在
首次 WRITE 前终止。受损测试卷随后重新格式化。

修复后在全新 generation 1 卷写入 `/session-events`：

```text
2 directories
2 files
614413 bytes
generation 1 -> 2
index chain (a,5) -> (b,11)
```

全部五类阶段事件按顺序出现；tapecpy 逐文件读回 SHA-256 与源文件一致。
OpenLTFS full medium consistency check 后成功挂载 generation 2，识别完整目录树，
并读出相同 SHA-256。安全卸载 OpenLTFS 后再次由 tapecpy 写入单文件，
generation 2→3，index 分区同步完成且 label 保持可解析，覆盖了最初暴露问题的
跨程序驱动器状态场景。
