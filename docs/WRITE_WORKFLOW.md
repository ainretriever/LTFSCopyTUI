# LTFS 正常写入工作流

本文档记录 tapecpy 第一阶段需要完整支持的实际 LTFS 写入流程。

这不是理想化的软件功能列表，而是用户当前使用 LTFSCopyGUI 时真实执行的工作流程。

它同时作为 tapecpy 第一阶段最重要的端到端验收场景。

## 1. 选择磁带机

启动 tapecpy。

列出当前系统识别到的磁带机，并确认本次准备操作哪一台设备。

选中的磁带机成为当前 session 的设备上下文。

后续所有磁带操作和诊断查询均针对这台设备。

## 2. 检查磁带和磁带机

检查当前装载的介质以及磁带机状态。

由于实际使用中经常处理二手 LTO 磁带，因此这里不仅需要确认磁带是否存在，还需要尽可能提供：

* LTO 代际；
* 介质类型；
* 可写状态；
* 当前 barcode；
* 当前 LTFS volume name；
* 当前格式；
* partition 信息；
* TapeAlert；
* 错误计数；
* 其他可取得的健康信息。

## 3. 擦除和准备磁带

根据实际情况选择磁带准备方式。

需要支持：

### 短擦除

快速使原有数据不可正常访问。

### 完整长擦除

让磁带机执行完整的 long erase。

### 最小分区长擦除

创建一个尽可能小的磁带分区，然后对这个分区执行 long erase。

实际用途是让磁带运行少量 wrap，以较低时间成本对二手介质进行一定程度的物理检查和重新写入。

这一操作属于介质擦除/准备，不应与使用 cleaning cartridge 清洁磁带机混淆。

## 4. LTFS 格式化

将磁带重新格式化为 LTFS。

用户需要设置：

* 6 位 Volume Serial；
* LTFS Volume Name。

例如：

```text
Volume Serial: E62115
Media ID: L5（由已装载介质自动推导，只读）
Physical Barcode: E62115L5
Volume Name: e621 Archive 15
```

TUI 只允许 6 位 ASCII 字母数字 Volume Serial，并自动转为大写。完整 8 位物理
Barcode 由 Volume Serial 和介质代际码组合；ANSI label 与 MAM 保存 6 位 serial，
不能把派生的 Media ID 混入设备数据。Barcode 在整个后续工作流程中应保持明显可见。

格式化入口要求磁带已装载并绕带、未写保护、没有活动 Read/Write 任务，且介质密度
能够可靠映射到 Media ID。开始前必须显示独立的最终破坏性确认。当前 Format 由 TUI
device worker 持有统一设备 lease 执行，不是可脱离任务；运行期间禁止退出和其他设备
操作。完成后应重新读取 LTFS volume，显示 generation、UUID、Volume Name 和物理
Barcode，不能只凭命令返回成功就结束。

## 5. 选择写入内容

选择准备写入磁带的数据。

包括：

* 单个文件；
* 多个文件；
* 目录树。

第一阶段主要关注正常 LTFS 数据写入。

## 6. 写入

开始向 LTFS volume 写入数据。

专用持续写入/遥测测试可以使用 `write-random <大小> <磁带路径> [--seed=N]`
产生已知长度、可重现且不占用磁盘空间的伪随机流。该入口仍经过普通 LTFS
writer，并生成 extent、SHA-256、两个 index 和 MAM VCI；它不是普通归档文件
选择方式，也不改变正式 write workflow 的语义。

写入过程中需要持续显示：

### 数据传输状态

* 当前文件；
* 当前文件进度；
* 总进度；
* 当前速度；
* 速度历史；
* 已写入容量。

### 磁带状态

* 当前 partition；
* 当前 logical position；
* 相关设备状态。

### 介质健康信息

尽可能取得并显示：

* recovered write errors；
* recovered read errors；
* hard write errors；
* hard read errors；
* TapeAlert；
* 其他与写入质量有关的诊断统计。

由于使用大量二手磁带，这些数据不仅用于观察，而是用于判断当前磁带是否值得继续使用。

tapecpy 应该向用户提供充分信息和明显告警，但第一阶段不应擅自根据某个固定阈值宣布磁带报废。

最终判断由用户作出。

## 7. 写入完成后的 LTFS index 更新

最后一个普通文件写完，并不代表整个任务完成。

程序必须明确进入 LTFS finalization 阶段。

例如状态可以为：

```text
WRITING_DATA
→
FINALIZING_INDEX
→
SYNCING
→
WRITE_COMPLETE
```

TUI 必须明确告诉用户当前正在更新或同步 LTFS index。

不能把这一过程隐藏在普通的“复制完成”状态中。

## 8. 可选完整校验

对于特别重要的数据，可以在写入完成之后重新读取磁带上的内容进行完整校验。

正常归档写入不一定每次进行这一操作。

需要明确区分：

### 写入时 hash

写入过程中计算源数据 hash。

当前实现固定计算 SHA-256，并把同一条写入数据流的摘要保存为 LTFS 2.4
扩展属性 `ltfs.hash.sha256sum`（64 个小写十六进制字符）。它不会为了 hash
再次读取源文件，因此摘要描述的是实际交给磁带写命令的数据。解析和重写 index
时必须保留已有文件/目录扩展属性，不能只保留 tapecpy 自己生成的 hash。

### Read-back verify

真正重新读取已经写入磁带的数据，并与原始数据或预期 hash 比较。

只有后者能够提供完整的应用层写后读取验证。

CLI 通过显式选项 `--read-back-verify` 启用这一行为。校验发生在 data/index
两个 index 和 MAM VCI 均成功提交之后，使用本次 index 中的 extent 从磁带回读，
再与写入时 SHA-256 比较。因此校验失败表示“写入已经提交，但回读校验失败”，
不能把它误报成卷没有更新；该模式会额外产生定位和全量读取开销。

## 9. 弹出磁带

所有要求的操作完成后，安全 unload / eject 磁带。

索引更新不是可选行为：正常 Write 必须先提交 data/index 两份 index 和 MAM VCI，
才能报告写入完成。用户可以选择完成策略：默认在提交（以及可选 read-back verify）
后保持装载，或选择 `EjectAfterCommit` 自动执行直接 `Eject`。自动弹出同样由
detached runner 执行，不依赖 TUI 或 SSH 存活。

自动弹出的顺序固定为：

```text
写入数据 → 提交两个 index → 更新 MAM VCI → 可选 read-back verify → 可选 eject
```

index/VCI 提交失败时禁止自动弹出并要求诊断；read-back verify 失败表示写入已经
提交，但默认保留介质供诊断；eject 失败则报告“写入成功、弹出失败”，不能把已经
提交的写入误报为失败。

弹出以后仍然明显显示：

```text
Barcode
LTFS Volume Name
任务结果
校验结果
```

## 10. 物理贴标

磁带弹出后，根据程序中设置的 Barcode：

* 贴上新的条形码标签；或者
* 使用记号笔在磁带上标记 Barcode。

程序应在最后的完成界面再次明确显示 Barcode，以降低逻辑卷编号和物理磁带标签不一致的风险。

## 第一阶段验收标准

在 Linux 系统中：

1. 插入一盘二手 LTO 磁带；
2. 启动 tapecpy；
3. 选择磁带机；
4. 查看介质和设备状态；
5. 执行选定的擦除方案；
6. 创建新的 LTFS volume；
7. 设置 Barcode 和 Volume Name；
8. 选择一个目录；
9. 完整写入；
10. 写入过程中观察速度和磁带错误统计；
11. 完成最终 LTFS index；
12. 可选执行完整 read-back verify；
13. 安全弹出；
14. 根据显示的 Barcode 完成物理贴标。

这条流程能够稳定完成时，视为 tapecpy 第一阶段核心目标达成。
