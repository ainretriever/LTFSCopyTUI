# tapecpy 架构设计

版本：0.1
状态：初始架构基线

## 1. 文档目的

本文档描述 tapecpy 当前已经确定的软件架构原则。

它的目的不是提前规定所有 class、module、thread、queue 或 API，而是确定在重新实现 tapecpy 时不能违反的结构性约束。

tapecpy 曾经存在一个早期 prototype。该版本在缺少完整需求和架构规划的情况下逐步增加功能，最终变得难以理解和维护。

本次开发视为一次重新开始。

**新的 tapecpy 从头实现，不以旧代码为基础进行重构，也不继续扩展旧代码。**

旧 prototype 可以保留用于历史参考或验证过去做过的实验，但 Codex 不应从旧代码复制架构、模块划分或状态管理方式。

新的需求文档和本文档是当前实现的主要依据。

---

# 2. 项目架构目标

tapecpy 最初的构想可以概括为：

> LTFSCopyTUI：一个面向 Linux、能够直接操作 LTFS 和 LTO 磁带机的 TUI 工具。

tapecpy 不依赖把 LTFS 通过 FUSE 挂载成普通文件系统来完成核心操作。

程序应直接理解 LTFS，并直接控制磁带设备。

tapecpy 的主要架构目标包括：

1. 让 LTFS 操作过程保持可观察；
2. 明确展示磁带机状态，而不是把磁带隐藏在普通文件系统抽象后面；
3. 将磁带设备操作、LTFS 格式处理和用户界面分离；
4. 保证 LTFS 格式逻辑能够尽可能脱离真实磁带机进行测试；
5. 为以后加入 RAW、TAR 和 recovery 功能留下空间；
6. 保持第一阶段范围足够小，避免再次出现功能无序扩张。

---

# 3. 当前平台范围

第一阶段只支持 Linux。

当前不考虑：

* Windows；
* macOS；
* 跨平台磁带设备 abstraction。

如果为了测试、模拟设备或隔离 Linux 设备访问而需要建立接口，可以建立适当 abstraction。

但不能仅仅为了未来可能支持其他操作系统而提前构造复杂的平台层。

# 实现语言

tapecpy 的正式实现使用 Rust。

选择 Rust 的主要原因不是单纯追求性能，而是：

- tapecpy 长期运行并操作具有破坏性的有状态设备；
- 需要精确定义 SCSI、LTFS 和设备状态数据结构；
- 需要可靠管理 buffer、并发和设备状态所有权；
- 希望最终以原生 executable 的形式部署；
- Rust 现有生态能够满足 Linux ioctl、XML 和 TUI 的需求。

Python 不作为 tapecpy production implementation language。

允许在 `experiments/` 中使用 Python 编写一次性硬件实验程序，例如：

- 测试 SCSI command；
- dump LOG SENSE；
- 检查 MAM；
- 验证 ioctl 行为；
- 分析二进制响应。

实验程序不得成为正式 tapecpy 的运行时依赖。
实验得到的稳定结论应重新实现到 Rust core，并建立相应测试。

---

# 4. 开发方式：垂直切片

tapecpy 不采用先完成所有底层、再完成所有 LTFS、最后再开发 TUI 的方式。

项目采用 **垂直切片开发**。

每个阶段都应该尽可能形成一条从用户界面到真实磁带设备的完整可运行路径。

例如：

```text
TUI
 ↓
应用操作
 ↓
LTFS
 ↓
磁带设备访问
 ↓
真实磁带机
```

第一阶段不追求完整实现所有底层能力，而优先完成越来越完整的真实工作流。

推荐的发展方式类似：

```text
发现并选择磁带机
        ↓
读取和显示磁带基本信息
        ↓
识别 LTFS volume
        ↓
读取 LTFS index
        ↓
显示目录
        ↓
写入一个文件
        ↓
写入多个文件和目录
        ↓
完成 LTFS index finalization
        ↓
形成完整 LTFS 写入 workflow
```

每完成一个阶段，都应该能够在真实设备上验证这一整条路径。

不要为了“以后可能需要”而提前横向实现大量尚未被当前 workflow 使用的功能。

---

# 5. 第一阶段的主要架构驱动力

第一阶段最重要的用户工作流是正常 LTFS 写入。

详细需求见：

`docs/WRITE_WORKFLOW.md`

其基本过程为：

```text
选择磁带机
→
检查介质
→
擦除 / 准备磁带
→
LTFS 格式化
→
设置 Barcode 和 Volume Name
→
选择写入数据
→
写入
→
监控速度和错误情况
→
写入最终 LTFS index
→
可选 read-back verify
→
弹出磁带
```

架构设计应首先服务于这条工作流。

RAW、TAR、LTFS recovery 等功能不得阻塞这条工作流的完成。

---

# 6. 总体分层原则

当前只确定逻辑上的分层关系，不提前规定具体目录和 class。

总体关系为：

```text
┌──────────────────────────────────┐
│          Presentation            │
│                                  │
│             TUI / CLI            │
└────────────────┬─────────────────┘
                 │
                 ▼
┌──────────────────────────────────┐
│          Application             │
│                                  │
│ 用户操作和工作流                  │
└────────────────┬─────────────────┘
                 │
                 ▼
┌──────────────────────────────────┐
│       Format / Tape Logic        │
│                                  │
│ LTFS             Telemetry       │
│ RAW / TAR        Verify          │
└────────────────┬─────────────────┘
                 │
                 ▼
┌──────────────────────────────────┐
│       Tape Device Control        │
│                                  │
│ Linux tape API / SCSI            │
└────────────────┬─────────────────┘
                 │
                 ▼
              LTO Drive
```

这些层次表达的是依赖关系，而不是要求一层必须对应一个 Python package 或一个 class。

具体代码结构应随着第一版实现逐渐确定。

---

# 7. 依赖方向

总体依赖方向必须保持：

```text
Presentation
      ↓
Application
      ↓
Format / Tape Logic
      ↓
Device Access
```

下层不得依赖上层。

例如：

* Linux 磁带设备访问代码不能知道 TUI 的存在；
* LTFS XML parser 不能依赖 TUI；
* 磁带读取代码不能直接更新 progress bar；
* LTFS writer 不能直接打印面向用户的文本；
* TUI 不应该直接发送 SCSI CDB；
* CLI 不应该直接调用 Linux ioctl 绕过核心逻辑。

用户界面负责显示和发出操作请求。

核心逻辑负责执行操作并返回结构化状态。

---

# 8. 用户界面

TUI 是 tapecpy 的主要交互界面。

CLI 用于：

* 自动化；
* 脚本；
* 调试；
* 状态查询；
* stdin/stdout；
* 非交互使用。

TUI 和 CLI 必须使用同一套核心逻辑。

不得维护两套独立的磁带操作实现。

正确关系：

```text
        TUI
         │
         ├─────┐
         │     │
        CLI    │
         │     │
         └──┬──┘
            ↓
         Core logic
```

禁止：

```text
TUI → 一套磁带实现

CLI → 另一套磁带实现
```

---

# 9. 磁带设备

磁带机是有状态的顺序设备。

tapecpy 必须承认这一特征，而不能把磁带完全抽象成普通随机访问文件。

设备访问层负责实际与 Linux 磁带设备及 SCSI 接口通信。

预计可能涉及：

```text
/dev/nstX
/dev/sgX

Linux st driver
ioctl
SG_IO
SCSI CDB
```

具体采用哪些接口，由实际功能需求决定。

设备访问层可以提供的能力包括但不限于：

* open / close；
* read / write；
* rewind；
* space；
* locate；
* read position；
* partition control；
* write filemark；
* erase；
* load；
* unload；
* SCSI command；
* LOG SENSE；
* MODE SENSE；
* READ ATTRIBUTE / MAM；
* REQUEST SENSE。

该层不知道什么是：

* LTFS 文件；
* LTFS 目录；
* Barcode 写入 workflow；
* TUI 页面；
* 用户正在执行什么业务任务。

---

# 10. 磁带机身份与设备路径

`/dev/nst0`、`/dev/sg4` 等路径只是访问磁带机的入口，不应被视为磁带机永久身份。

程序应尽可能取得：

* Vendor；
* Model；
* Serial Number；
* 对应的 tape device path；
* 对应的 generic SCSI device path。

同一台物理磁带机可能同时对应多个 Linux device node。

上层应把它们理解为同一台设备。

具体如何发现和匹配这些路径，可以在设备发现功能实现过程中确定。

---

# 11. 设备状态所有权

同一台磁带机的状态性操作必须受到统一管理。

LTFS、RAW、TAR、Telemetry 等组件不能各自随意打开设备并独立改变磁带状态。

需要被统一管理的操作包括：

* partition 切换；
* locate；
* rewind；
* read；
* write；
* erase；
* unload；
* 其他可能影响当前磁带状态的 SCSI command。

当前只确定：

> 同一台磁带机必须存在统一的设备状态所有权。

暂时不规定具体实现必须是：

* `TapeSession` class；
* 独立线程；
* command queue；
* mutex；
* async task；
* 其他并发模型。

这些实现方式应在完成第一版真实设备访问之后，根据实际情况决定。

如果实现中使用 `TapeSession` 这一名称，它应表示一次针对某台磁带机的有状态操作会话，而不是简单的函数集合。

## 11.1 SG_IO 磁带命令超时

当前设备层不能对所有 SG_IO 命令统一使用短超时。

在 Quantum ULTRIUM 5 真实设备测试中，WRITE(6) 和 WRITE FILEMARK(6)
正常执行时间超过 10 秒。原实现统一使用 10 秒超时，导致 Linux SCSI 层对
仍在正常执行的命令发起 task abort。由于代码当时只检查 SCSI status、没有
同时检查 host status 和 driver status，该 task abort 还可能被误判为成功，
进而继续更新 LTFS 状态。

当前实现采用：

```text
普通查询命令：10 秒

磁带运动、数据通道及落带命令：1800 秒
```

长超时当前适用于：

* READ；
* WRITE；
* WRITE FILEMARK；
* LOCATE；
* SPACE；
* REWIND；
* LOAD / UNLOAD；
* MODE SELECT（设置磁带块模式等设备状态）。

1800 秒取自 LTFSCopyGUI 对 LOCATE 使用的超时上限，并作为当前真实设备验证
阶段的保守值。它不是已经证明适合所有命令、所有驱动器的最终配置。

这个决定存在明确风险：

* 真正卡死或失去响应的命令可能很久以后才返回；
* cancellation 和程序退出可能长时间等待内核中的 SG_IO；
* TUI 如果没有独立的进度与设备状态报告，可能表现得像程序冻结；
* 不同命令的合理超时差异很大，统一使用 1800 秒可能掩盖设备故障；
* 某些 HBA、内核驱动或设备自身还有独立的超时与错误恢复机制，SG_IO
  timeout 并不是唯一的时间限制。

未来出现以下现象时，应优先重新检查本节和 `TAPE_TIMEOUT_MS`：

* 操作取消或退出长时间无响应；
* 设备故障需要很久才能报告；
* TUI 长时间停留在同一阶段；
* SCSI error recovery 行为异常；
* 不同型号磁带机表现出明显不同的命令耗时；
* 需要为 telemetry polling、前台操作或后台任务提供不同响应保证。

后续方向应是根据命令类型、设备能力和实测数据建立更细粒度的 timeout
policy，并让长命令具备可观察的阶段、耗时和取消语义。在此之前，不要在没有
真实设备复验的情况下把这些命令恢复成统一的 10 秒超时。

### 11.2 LTFS 覆盖写必须显式确认 write-anywhere 模式

LTFS 刷新 index 分区不是在 EOD 追加，而是定位到旧 index 前的 filemark
并覆盖。真实 Quantum LTO-5 测试确认：驱动器处于 append-only 模式时，
`LOCATE` 可以返回成功，但后续写入可能仍落到该分区 EOD，从而从块 0 覆盖
index 分区 label。因此不能把其他程序留下的驱动器模式当作隐含前置条件。

每个写入会话在首次写磁带前必须读取 SSC device configuration extension
mode page `0x10/0x01`。若 append-only 字段非零，应按 OpenLTFS 的顺序无弹出
卸载介质、用 MODE SELECT(10) 清除 append-only、重新装载，并恢复可变块模式。
此步骤失败时必须拒绝写入。

---

# 12. Telemetry 与设备控制

Telemetry 是 tapecpy 的核心功能，而不是单纯的 TUI 装饰。

tapecpy 希望观察：

* 当前写入速度；
* 速度历史；
* recovered write error；
* recovered read error；
* hard write error；
* hard read error；
* TapeAlert；
* drive/media statistics；
* 当前 partition；
* 当前 position；
* 其他能够反映磁带写入状态的数据。

Telemetry 不拥有磁带机。

它不能为了刷新数据显示而绕过统一的设备状态管理机制。

正确关系应类似：

```text
Telemetry
    │
    │ 请求状态
    ▼
统一设备控制
    │
    ▼
磁带机
```

而不是：

```text
LTFS Writer ───→ 磁带机
Telemetry  ───→ 磁带机
TUI        ───→ 磁带机
```

写入期间的第一版实现由统一写入会话在安全命令边界每 5 秒采样一次。实时吞吐
按相邻成功采样点间的有效载荷字节差和实际时间差计算，与通道错误率共享时间戳、
partition 和 position，并使用同一份 10 分钟滚动历史。它不统计会话平均速度。

---

# 13. Telemetry 数据与累计计数器

磁带机返回的部分统计数据可能是累计计数器。

因此应区分：

```text
设备原始计数器

和

当前操作期间的变化量
```

例如：

```text
任务开始：
Recovered Write Errors = 152000

当前：
Recovered Write Errors = 152120

本次任务：
Recovered Write Errors = 120
```

用户通常更关心当前 session 或当前操作期间的数据。

因此 telemetry 设计必须允许记录 baseline。

具体数据模型在开始实现 LOG SENSE 后确定。

当前第一版数据模型读取 cumulative-values LOG SENSE page：写错误 02h、读错误
03h、TapeAlert 2Eh。设备层保留原始累计计数；Application 层在写入会话开始和
完成时各取一次快照，并用 `checked_sub` 形成操作期间差值。若计数下降（驱动器
重置、介质/统计域变化或回绕）则差值为 unknown，不能用饱和减法伪报为 0。

首版在任务开始和结束读取完整健康快照，并在数据写入的安全记录边界周期采样。
所有查询都复用统一 `TapeSession`，不能由 telemetry 线程另开句柄并与写入并发
发送命令。

面向用户和社区交流的主要“通道错误率”必须兼容 LTFSCopyGUI：读取厂商
RECEIVE DIAGNOSTIC RESULTS page 88h（write）/87h（read），以相邻样本的 C1
error 与 CCP 差值计算 `log10(ΔC1 / ΔCCP / 2 / 1920)`。标准 LOG SENSE
corrected/uncorrected counters 继续保留为另一组原始诊断数据，但不得用它们计算
或命名为 LTFSCopyGUI 通道错误率。

通道错误率的目标采样间隔为 5 秒。TUI 只保留最近 10 分钟滚动历史（最多
120 个样本），默认显示最近 5 分钟（60 个样本）。不照搬 LTFSCopyGUI 为速度
曲线设置的 6 小时容量，因为通道错误率的主要用途是观察近期变化，而不是事后
回看数小时的逐点数据。

滚动历史之外，整个写入会话仍应保留最差通道错误率摘要，包括数值、通道、
采样时间和当时的 partition/logical position。这样长任务完成后仍能报告早先的
最差情况，而无需保存完整的长时间曲线。

---

# 14. LTFS

LTFS 是 tapecpy 第一阶段最重要的 format。

tapecpy 自己实现 LTFS 的读取和写入，而不是依赖已经挂载的 FUSE LTFS filesystem。

LTFS 实现至少最终需要理解：

* LTFS Label；
* Index Partition；
* Data Partition；
* LTFS Index；
* directory；
* file；
* extent；
* logical block；
* index generation；
* formatting；
* index update；
* finalization。

tapecpy 不创建私有 LTFS 变体。

所有写入的数据必须尽可能遵守正式 LTFS 规范。

---

# 15. LTFS 互操作目标

tapecpy 的目标不是做到：

> tapecpy 写出的磁带只能被 tapecpy 自己读取。

必须以与其他 LTFS 实现互操作为目标。

至少需要逐步验证：

```text
tapecpy 写入
      ↓
OpenLTFS / HPE LTFS 等实现可以读取
```

以及：

```text
其他 LTFS 实现写入
      ↓
tapecpy 可以读取
```

因此：

**“tapecpy 可以读回 tapecpy 自己写出的磁带”不能作为充分的 LTFS 正确性证明。**

互操作测试是 LTFS 实现的重要验收方式。

---

# 16. LTFS 格式逻辑与设备 I/O 分离

能够脱离磁带机工作的 LTFS 逻辑，应尽可能保持为纯数据处理。

例如：

```text
LTFS Index XML
      ↓
Parser
      ↓
内部数据结构
```

或者：

```text
内部数据结构
      ↓
Serializer
      ↓
LTFS Index XML
```

这些操作原则上不应该要求连接真实磁带机。

同样，能够离线处理的内容包括：

* label encoding/decoding；
* index parsing；
* index serialization；
* directory tree；
* extent metadata；
* 数据格式验证。

这样可以大量使用普通 unit test。

不要把 XML parsing、SCSI command 和 TUI 更新写在同一个函数中。

---

# 17. RAW

RAW 不是第一阶段优先功能。

未来 RAW 模式提供最直接的顺序磁带 I/O。

原则为：

```text
binary stream
      ↓
tape
```

多个独立输入对象可以使用 filemark 分隔。

RAW 本身不保存：

* 文件名；
* 路径；
* 时间戳；
* 权限；
* tapecpy 私有 metadata。

不要为 RAW 创建 tapecpy 私有 archive format。

---

# 18. TAR

TAR 也不是第一阶段优先功能。

TAR 应被视为普通顺序数据流上面的 archive codec。

概念关系为：

```text
Files / Directories
        ↓
       TAR
        ↓
Sequential Tape Stream
```

TAR 不应发展成第二套复杂的磁带文件系统。

其磁带数据通路应尽可能复用未来 RAW 模式的顺序 I/O 能力。

---

# 19. Recovery

LTFS recovery 属于未来功能。

正常 LTFS 读取失败时，未来可能提供：

* 扫描旧 LTFS index；
* 查找可恢复 index generation；
* 扫描 data partition；
* 根据 extent 恢复数据；
* partition raw dump；
* 保存 logical block / filemark / error map。

Recovery 与正常 RAW 模式不是同一个概念。

第一阶段不得为了 recovery 大量增加架构复杂度。

---

# 20. 应用 Workflow

用户执行的是一个完整操作，而不是一组互不相关的底层命令。

例如正常 LTFS 写入：

```text
Inspect
 ↓
Prepare
 ↓
Format
 ↓
Select data
 ↓
Write
 ↓
Finalize
 ↓
Optional Verify
 ↓
Eject
```

Application 层负责描述这种工作流。

Presentation 层负责：

* 让用户选择操作；
* 展示当前状态；
* 请求取消或确认。

Format/device 层负责真正执行操作。

禁止让 TUI page 或 button 自己承担磁带操作逻辑。

---

# 21. 可观察性

tapecpy 的重要设计目标之一是减少黑盒行为。

任何较长时间的操作都应该尽可能明确展示当前阶段。

例如写入过程至少应该能够区分：

```text
PREPARING
FORMATTING
WRITING_DATA
FINALIZING_INDEX
VERIFYING
UNLOADING
COMPLETED
```

这里的具体状态名称不是强制 API。

重要原则是：

**“最后一个文件已经写完”不等于“LTFS 写入任务已经完成”。**

LTFS index update、sync、verify、unload 等过程需要对用户保持可见。

---

# 22. Progress 与事件

核心逻辑不能通过直接打印文本来向用户报告进度。

例如不应该在 LTFS writer 内部出现：

```python
print("Writing...")
```

核心应提供结构化状态或事件。

以后可能包括：

```text
OperationStarted
OperationProgress
FileStarted
FileCompleted
TelemetryUpdated
WarningRaised
OperationFailed
OperationCompleted
```

TUI 可以把这些信息转换成：

* progress bar；
* table；
* graph；
* warning dialog。

CLI 可以把同样的信息转换成：

* 文本；
* JSON；
* machine-readable output。

具体事件模型在实现第一条完整垂直切片时确定。

---

# 23. 错误透明性

tapecpy 不应把所有底层故障最终压缩成：

```text
I/O Error
```

只要底层能够取得，错误信息应尽可能保留：

* 当前操作；
* 当前 workflow 阶段；
* 当前文件；
* partition；
* logical position；
* SCSI command；
* Sense Key；
* ASC；
* ASCQ；
* raw sense data；
* TapeAlert；
* 其他诊断信息。

TUI 可以向普通用户提供简化错误说明。

但底层详细信息必须保留在日志或诊断信息中。

这是 tapecpy 去黑盒化目标的一部分。

---

# 24. 破坏性操作安全

以下操作属于明显的破坏性操作：

* erase；
* format；
* partition modification；
* 其他可能破坏已有磁带内容的操作。

TUI 必须在执行之前明确显示当前磁带信息，并要求用户确认。

应尽可能显示：

* 当前选择的磁带机；
* Vendor / Model / Serial；
* 当前介质；
* Barcode；
* LTFS Volume Name；
* 即将执行的操作类型。

CLI 中的破坏性操作也必须采用明确的命令语义，不能因为默认参数或模糊命令意外触发擦除。

具体确认交互方式以后决定。

---

# 25. Barcode 与 Volume Name

Barcode 和 LTFS Volume Name 是 LTFS workflow 中的重要介质身份信息。

它们不能只在 format 对话框中短暂出现。

在写入 session 中，应尽可能保持当前 Barcode 可见。

任务完成、磁带 eject 后，也应该再次明确显示 Barcode，方便用户进行物理贴标。

物理磁带与逻辑 volume identity 不一致属于需要尽量避免的人为错误。

### Barcode 的两种表示

MAM attribute 0x0806（barcode）只保存写入它的软件所给的原值。
tapecpy 读取时原样展示，不做拼接或猜测，也不把推导结果写回 MAM。

LTO 物理标签的标准格式是 8 位大写字母数字：

```text
前 6 位 = 卷序列（volume serial）
后 2 位 = 介质代际码（LTO 数据磁带为 L1-L9；LTO-7 M8 为 M8；
          WORM 磁带另有 LT/LU/LV 等变体）
```

因此同一盘磁带可能同时表现为两种形式：

* MAM barcode = 6 位卷序列，例如 `E6008A`；
* 物理标签 = 8 位完整条码，例如 `E6008AL5`。

tapecpy 的处理规则：

1. 读取：原样显示 MAM barcode；当 barcode 为 6 位且密度表明是 LTO 时，
   显示推导出的 8 位标准标签（如 `E6008AL5`）作为核对物理标签的提示，
   并明确标注这是推导结果而非设备数据。
2. 写入（LTFS 格式化时）：接受用户输入的 barcode 原样写入 MAM，不自动补
   代际码；若输入为 8 位，校验后两位是否为合法的介质代际码。
3. 身份比较：MAM 6 位 barcode 与物理 8 位标签视为同一盘磁带
   （6 位卷序列前缀匹配）；代际码与介质代际不一致时给出警告。

---

# 26. Verify

校验不能简单表示成一个 `verify = true/false`。

需要至少在概念上区分：

### 写入过程中的 Hash

对源数据计算 hash。

这能证明输入的数据内容，但不能证明磁带能够重新读取相同数据。

当前垂直切片选择 SHA-256，在数据块成功写入时更新摘要，并以 LTFS 标准扩展
属性 `ltfs.hash.sha256sum` 写入 index。index 模型会往返保留其他已有扩展属性。

### Read-back Verify

写入完成后，重新读取磁带数据并进行比较。

这是面向重要数据的可选完整校验。

当前应用层用 `WriteVerification::ReadBackSha256` 明确表示这种模式；CLI 对应
`--read-back-verify`。它在 index 和 MAM VCI 提交后使用同一 `TapeSession` 按
extent 回读，失败必须报告“写入已提交但校验失败”。

未来还可能研究：

* SCSI VERIFY；
* drive-level media verification。

但这些不同机制不能混为一个含义不明的 `verify` 参数。

---

# 27. 中断与恢复

第一阶段不实现复杂的断点续写。

如果写入任务发生意外中断，可以要求用户重新开始操作。

但是程序仍然必须尽可能安全地处理中断请求。

不应简单通过粗暴杀死进程来实现正常 cancellation。

第一版至少应逐渐做到：

```text
用户请求取消
      ↓
停止开始新的高层操作
      ↓
尽可能结束当前安全操作单元
      ↓
记录当前状态和位置
      ↓
报告任务未完成
```

具体 cancellation granularity 根据真实写入实现决定。

---

# 28. 测试原则

测试分为三个层次。

## 28.1 纯软件测试

不需要磁带机。

用于测试：

* LTFS XML parsing；
* LTFS XML serialization；
* label；
* directory tree；
* extent metadata；
* state transformation；
* TAR；
* 其他纯数据逻辑。

这是日常开发中最重要的测试类型。

## 28.2 模拟设备测试

以后可以提供 fake/mock tape backend。

用于测试：

* workflow；
* 错误处理；
* 状态变化；
* progress；
* cancellation；
* 特定设备错误场景。

建立 fake backend 的目的主要是测试，而不是跨平台。

## 28.3 真实磁带机集成测试

用于验证：

* Linux tape API；
* SCSI command；
* partition；
* position；
  -真实 read/write；
* erase；
* format；
* LTFS interoperability；
* telemetry；
* 特定驱动器行为。

真实磁带测试不能替代 unit test。

unit test 也不能替代真实磁带测试。

---

# 29. LTFSCopyGUI 的架构地位

LTFSCopyGUI 是 tapecpy 的重要参考实现。

参考源码位于：

`references/LTFSCopyGUI/`

在实现以下能力时，应主动研究其相关实现：

* LTFS format；
* LTFS index；
* extent；
* partition；
* SCSI command；
* MAM；
* LOG SENSE；
* TapeAlert；
* erase；
* position；
* capacity；
* verify；
* 驱动器兼容处理。

但 LTFSCopyGUI 不定义 tapecpy 的架构。

必须区分：

```text
它实现了什么行为

和

它为什么以这种程序结构实现
```

tapecpy 可以参考：

* 命令序列；
* SCSI CDB；
* 数据字段；
* LTFS 格式行为；
* 磁带机 workaround；
* 实际硬件经验。

不要机械复制：

* WinForms 架构；
* Windows device API；
* 全局状态；
* UI 与核心逻辑耦合；
* VB 特有的程序组织方式。

如果某项行为来自 LTFS、SCSI 或设备厂商规范，应优先理解相应规范，而不是把 LTFSCopyGUI 的实现细节当成规范本身。

---

# 30. 旧 tapecpy prototype

旧 tapecpy 代码不再作为新版实现基础。

Codex 在实现新版时：

* 不要继续修改旧架构；
* 不要尝试渐进式重构成新版；
* 不要因为旧代码已经存在，就保留旧 module boundaries；
* 不要默认旧代码的 API 是兼容约束；
* 不要复制旧状态管理方式。

必要时可以研究旧代码以确认过去做过的实验或硬件行为，但新的实现应从新的目录和新的设计开始。

如果旧代码与当前需求文档或本文档存在冲突，以当前文档为准。

---

# 31. 当前明确不做的事情

第一阶段不实现：

* FUSE mount；
* POSIX filesystem compatibility layer；
* multi-volume spanning；
* tape library robot management；
* global tape catalog；
* 自动选择下一盘磁带；
* backup policy；
* incremental backup；
* deduplication；
* scheduler；
* 网络服务；
* Web UI；
* 自动判定二手磁带报废；
* 复杂 write resume；
* 完整 LTFS forensic recovery。

不要因为其中某项功能容易实现而提前加入。

---

# 32. 第一阶段开发策略

新版代码应从空白结构开始。

最初不需要立即建立完整 package tree。

应该根据垂直切片逐渐增加真实需要的模块。

第一批开发工作的目标应该类似：

```text
Milestone 0
启动程序
→
发现 Linux 磁带机
→
选择一台磁带机
→
显示设备身份
```

随后：

```text
Milestone 1
选择磁带机
→
检测是否装载介质
→
读取基本介质信息
→
显示给用户
```

随后：

```text
Milestone 2
选择磁带机
→
识别 LTFS
→
读取 LTFS label/index
→
显示 volume 基本信息
```

随后逐渐加入：

```text
浏览
→
读取文件
→
写入文件
→
更新 index
→
format
→
erase
→
完整 write workflow
```

截至 2026-08-09，`format` 和 `erase` 垂直切片已经实现。format 已在 Quantum
LTO-5 上完成 tapecpy/OpenLTFS 交叉验证；erase 的 short 与最小分区 long
已经真实验证，全带 long 因耗时过长明确留待未来维护窗口测试。实现范围、参考
行为和兼容性边界见 `docs/REVIEW_FORMAT_WORKFLOW.md` 和
`docs/REVIEW_ERASE_WORKFLOW.md`。普通写入结束后的 MAM VCI 更新已实现并验证，
详见 `docs/REVIEW_MAM_WORKFLOW.md`；下一阶段继续处理完整 write workflow 的
校验、故障注入和恢复语义。Milestone 11 的提交状态、自动化故障注入、测试带
集成场景和真实故障带只读验收见 `docs/MILESTONE_11_TEST_MATRIX.md`；测试专用
磁带结果见 `docs/REVIEW_FAILURE_WORKFLOW.md`。

只读 `diagnose` 默认完整扫描较小的 index partition，并按 VCI/index chain 定点
读取 data partition index，避免大卷诊断隐式触发数小时全带读取。只有显式
`--full` 才允许顺序扫描完整 data partition。

实际 milestone 顺序允许根据实现过程中发现的问题调整。

关键要求是：

> 每个阶段优先形成完整可运行的垂直路径，而不是提前实现大量孤立的底层功能。

---

# 33. Milestone 12 TUI 垂直切片决定

第一版 TUI 使用 `ratatui` 与 `crossterm`。无参数启动 `tapecpy` 时进入 TUI，
已有 CLI 子命令继续保留，并与 TUI 共用 Application API。

设备访问采用单一后台 worker：TUI 只发送命令并接收不可变快照，redraw 和 page
切换不直接访问 `/dev/nstX` 或 `/dev/sgX`。worker 串行执行介质检查、LTFS 识别、
装卸载和健康采样，避免不同页面或 telemetry poller 同时改变顺序设备状态。

选择驱动器时 TUI 必须立即进入设备页，只异步取得基础介质与健康状态，不自动读取
LTFS label/index，也不因磁带定位把用户留在驱动器选择页。LTFS 区域在用户明确执行
`I Read LTFS` 前保持未读取状态；该命令才由 device worker 串行执行 LTFS 识别、
index 读取和一致性诊断。`R Basic refresh` 只刷新基础状态，并清除可能已经过期的
LTFS 快照。

当前稳定介质状态由 Application 层表达为 `NO_MEDIA_DETECTED`、
`PRESENT_UNTHREADED` 和 `LOADED_THREADED`，另保留 transitioning/unknown 状态。
Quantum LTO-5 实机证明：未穿带介质会以 TUR `3A/04` 报告，但此时仍可读取
MAM；因此不能只根据 TUR 的“not ready”结果断言没有介质。

通道错误率每 5 秒采样一次，采用与 LTFSCopyGUI 相同的计算方法。第一版界面
固定显示 4×4 的 16 通道实时矩阵，不显示历史曲线；采样失败时保留最后一次成功
值并标记 stale。

当前底层的 SCSI `UNLOAD` 同时完成 unthread/eject，尚不能诚实地提供两个独立
动作。因此第一版把它显示为 `Unload / Eject`，不伪造尚未实现的状态转换。

## 33.1 可脱离的长时间任务

进程内 device worker 适用于设备浏览、MAM、Health 和其他尚未进入数据传输的
操作。只有在用户最终确认并真正开始 LTFS Read 或 Write 时，才按 operation 创建
独立 job runner 进程；它不得依赖 TUI 进程或 SSH session 存活。启动 TUI 本身
不创建常驻 daemon；如果没有开始 Read/Write，退出 TUI 就直接退出且不遗留进程。

```text
TUI / CLI client ── Unix socket ──> tapecpy job runner ──> tape drive
       │                                  │
       └─ detach / reconnect              ├─ persisted state
                                          └─ event log
```

确认的语义如下：

* 用户确认 Read/Write 后才创建 runner；每个 operation 对应一个 runner；
* runner 创建新 session、脱离控制终端并把输出写入任务日志；
* TUI 关闭、客户端崩溃或 SSH 断开只表示 detach，不请求取消；
* 重新连接的客户端从持久化状态取得 job identity，再通过本机 Unix socket attach；
* runner 是任务期间唯一的设备 owner，并持有按驱动器序列号建立的排他锁；
* `cancel` 只设置 Application cancellation token；界面必须显示“已请求，等待安全
  停止点”，不能杀死 runner 或声称已经停止；
* 每次重要状态变化应先原子更新持久化状态，再通知客户端；
* 任务完成后 runner 退出，保留最终状态和日志，socket 随之消失；
* 主机或 runner 意外终止后，不自动从中途续写。残留 running 状态必须解释为
  interrupted，并要求执行一致性诊断。

TUI 的操作顺序固定为：先选择驱动器和 Read/Write 方向，再在方向专用的浏览界面
中解析 source 与 destination，完成冲突检查并展示 operation plan。只有确认页的
`Start` 创建 runner。Read 的 destination 必须是明确的文件或目录，不能是依赖
SSH/TUI 存活的 stdout；Write 的 source 同样必须解析为 runner 可独立重新打开的
已挂载文件系统路径。

Write 在选择 source 后先扫描并冻结 plan，以文件数和 payload bytes 作为整个任务
进度的 denominator。payload 超过当前 LTFS available capacity 的 90% 时必须警告，
超过 available 时阻止启动；capacity unknown 必须保持 Unknown 并要求显式确认。
runner 仍逐文件校验实际长度，防止计划后 source 变化。Read 则先读取可信 LTFS
index，再从 index tree 选择恢复对象并汇总 length，最后选择 Linux destination。

第一版 Write source selector 读取 `/proc/self/mountinfo`，把 `nfs`/`nfs4`、
`cifs`/`smb3` 等网络文件系统与其 remote source 明确显示，并把网络挂载排在本地
挂载之前。目录浏览只枚举当前一级；选择 source 后才由独立 filesystem worker
递归扫描，避免大型目录或慢速 NFS/CIFS 阻塞 TUI redraw。tapecpy 只使用已经挂载
的 Linux 路径，不负责保存凭据或自行 mount。当前 runner 垂直切片只支持一个
source root，多选必须在 writer 数据模型扩展后再开放，不能只在 TUI 中伪造。
新建 job 同时记录 host endpoint 的 filesystem type 和 mount source；runner 在
访问数据前重新核对挂载身份，防止 NFS/CIFS 消失后把裸露的本地 mount point 当成
原共享继续操作。旧 job 未记录这些字段时保持向后兼容。

首版按任务创建的 runner 解决 SSH/SIGHUP 和 TUI 生命周期耦合，不承诺主机重启
后续传。未来可以让 transient systemd unit 托管每个 operation，以获得明确的
session 独立性、资源限制和审计策略，但不需要常驻 tapecpy 服务。

## 33.2 Detached runner 实机结论

Quantum LTO-5 上的首个垂直测试使用 2 GiB source 完成了以下验证：

* 发起任务的 SSH 命令退出后，runner 的 PPID 变为 1，SID 为 runner 自身 PID，
  且没有 controlling TTY；
* 新 SSH session 能通过 Unix socket attach，并持续取得 bytes、partition、block
  和 write phase；
* 完成后两个 index 与两个 MAM VCI 都为同一 generation，诊断为 `Healthy`；
* 写入期间的 IPC cancel 先进入 `CancellationRequested`，然后在
  `AfterDataIndex` 安全边界收敛为 `Cancelled`，正确报告 `DataIndexOnly` 并要求诊断；
* 取消测试后的介质已重新格式化为空的健康 LTFS 卷，避免把故障状态留给后续测试。
* 随后完成 64 MiB detached Read：发起 SSH 退出后由新 session attach，恢复文件与
  source 的 SHA-256 均为
  `3b6a07d0d404fab4e23b6d34bc6696a6a312dd92821332385e5af7c01c421351`；
* Read 的状态持久化按 250 ms 节流，避免按每个 512 KiB tape record `fsync` 状态
  文件；终态仍强制记录精确最终字节数。

这些结果证明 SSH/TUI client 生命周期已不再决定 Write workflow 生命周期。TUI
接入时必须在 runner 启动后停止原有进程内 health poller 对该设备的访问，所有实时
状态改从 job snapshot 取得。

TUI 创建 runner 前使用带 acknowledgement 的 `Suspend` 完成设备所有权交接；只有
进程内 device worker 已停止 telemetry 并确认后才允许 spawn。超时则不创建 job，
避免“暂停命令尚在队列中、runner 已经访问设备”的竞争窗口。

Milestone 12 TUI 已增加 Jobs 页面：启动时发现 retained jobs，展示 active/terminal
状态、进度、位置、吞吐和 diagnosis 要求，并对 cancel 使用二次确认。检测到 active
job 的 drive serial 与当前设备相同时，TUI 暂停本地 telemetry，并禁止 refresh、
load 和 unload；选择被占用设备时直接进入对应 job，而不创建新的设备快照。

## 33.3 TUI 网络源写入实机结论

Quantum LTO-5 上使用 NFS source 完成了第一轮从 TUI 发起的端到端写入。测试数据
位于 `nfs4` 挂载点，包含媒体文件、可压缩小文件、目录以及中文文件名。TUI 冻结的
source plan 为 163 个文件、19 个目录、5,040,679,554 bytes；按当时 35.62 GiB 的
LTFS available capacity 计算为 13.2%，未触发 90% 警告。

测试确认：

* 网络挂载排在选择器前部，并同时显示 mount point、filesystem type 和 remote source；
* 当前一级目录浏览、后台递归扫描和 LTFS 目标目录选择没有阻塞 TUI redraw；
* 最终确认后，device worker 先完成 acknowledged `Suspend`，runner 再取得设备所有权；
* runner 启动后关闭 TUI 并结束原 SSH session，新 SSH session 查询时任务仍为
  `Running`，证明 TUI/SSH 生命周期没有控制写入生命周期；
* runner 重新核对 NFS mount identity 后写入全部 163 个文件，并分别报告
  `WritingData`、`FinalizingDataIndex` 和 `SyncingIndexPartition`；
* payload 完成时任务仍保持 `Finalizing`，直到两个 index 和 MAM VCI 提交完成才进入
  `Completed`；
* 最终两份 LTFS index 和两份 MAM VCI 均为 generation 3，数据 index 位于
  `p1b9851`，索引分区副本位于 `p0b5`，有界诊断结果为 `Healthy`；
* 新建 `/nfs-test` 目录可以从最终 index 列出，至少包括四个大媒体文件和包含小文件的
  子目录。

这轮测试验证的是当前 Write 垂直切片，不等同于整个 Milestone 13 完成。后续已经
接入 source I/O throughput、buffer occupancy 和 throughput graph；Milestone 13
仍需补齐独立完成页、可选 read-back verify 以及 safe unthread/eject 流程。

runner 的持久化快照保留最近 600 个 1 秒 tape-throughput 样本（10 分钟）、当前
16 通道 BER、BER 采样时间和不会随滚动窗口丢失的 session worst。Jobs 页默认绘制
最近 300 个性能样本（5 分钟）的全宽 Braille tape-throughput 图和 4×4 BER 矩阵；
重新 attach 不需要从零积累历史。

Write 数据路径使用一个 source reader 和一个唯一 tape writer，中间是默认 512 MiB
的 bounded buffer。source reader 按冻结计划顺序发送显式 `FileStart`、`Data` 和
`FileEnd`，只访问本地/NFS/CIFS 文件系统；tape writer 是唯一设备 owner，只有它能
写 record、维护 extent/block position，并对实际成功提交给磁带路径的数据计算
SHA-256。buffer 按 LTFS block 懒分配并循环复用，满时对 source reader 施加背压，
不会把完整文件载入内存或建立临时磁盘缓存。

性能采样和设备诊断分离：source/tape 区间吞吐及 buffer occupancy 每 1 秒采样，
BER 仍每 5 秒读取一次。任务快照分别保存 source bytes/s、成功 tape payload bytes/s、
buffer used/capacity 以及 reader/writer waiting 状态，因此 TUI 可以区分 source
starvation 和 tape 端消费较慢；不显示平均速度。

Quantum LTO-5/NFS 实机写入验证了 bounded pipeline：710 MiB 和 1.28 GiB 文件均由
512 MiB buffer 完整写入，任务快照取得独立 source/tape 速率、真实 buffer 占用、
1 秒历史和 16 通道 BER；文件结束时强制发布 buffer=0，避免 finalization 页面保留
最后一个周期的过期占用值。干净验收卷从 generation 1 提交到 generation 2，两份
index 和两份 VCI 一致，诊断为 `Healthy`。

实机测试还证明设备锁必须覆盖所有设备入口：一次在 runner
`SyncingIndexPartition` 尚未结束时启动的旧版 `diagnose` 与 MAM 更新发生设备竞争，
使任务正确降级为 `IndexesWritten / requires_diagnosis`，但测试卷需要重新格式化
恢复。现在 `device::lease` 为直接 CLI、TUI device worker 和 detached runner 提供
同一个按 drive serial 建立的非阻塞 `flock`。锁文件位于 XDG runtime/state 目录，
并记录 PID、owner kind、operation 和可选 job ID，冲突在发送介质、定位或数据命令
前即返回可诊断的占用信息。为取得稳定 serial 而执行的设备发现/只读 INQUIRY 位于
lease 之前；它不改变介质状态，后续若改用设备节点身份作为锁键可以进一步消除这个
例外。

直接 CLI 和 runner 分别在整条命令、整个 operation 生命周期持锁。TUI worker 在
每条设备命令的完整执行期持锁；telemetry 不能取得锁时不等待、不访问设备，而是保留
上一份 health snapshot、把通道采样标记为 stale 并报告当前 owner。`Suspend` 是
worker FIFO 队列中的 ownership barrier：acknowledgement 表示先前命令已经结束且
lease 已释放，进入 `Suspended` 后所有新设备命令和 telemetry 都被拒绝，直到
`Resume`。runner 终态不会触发一次可能早于进程退出的立即 refresh，worker 恢复后
由后续 telemetry 重试取得设备。

统一 lease 的 Quantum LTO-5 实机矩阵已经通过：CLI 长命令持锁时，TUI 的基本刷新和
telemetry 均在设备访问前被拒绝；TUI 执行 `Read LTFS` 时，另一个 CLI `health` 被
拒绝并显示 `kind=tui-worker operation=read-ltfs`；TUI acknowledged `Suspend` 后创建
Write runner，另一个 CLI `diagnose` 被拒绝并显示 `kind=job-runner operation=Write`
及 job ID。随后关闭 TUI 和原 SSH session，NFS source 仍写完 956,758,053 bytes，
任务进入 `Completed`；最终两份 index 和两份 VCI 均为 generation 3，有界诊断为
`Healthy`。

`Suspend` 释放 lease 与新 runner 取得 lease 之间并非跨进程原子交接；第三个进程若
恰好抢占该窗口，runner 会安全地因 lease 冲突失败，不会与它并发访问设备。未来若要
保证这种竞争下任务也必然启动，需要增加 runner-ready 握手或由常驻设备 broker
完成原子所有权转移。

当前 lock root 属于运行 tapecpy 的 Unix 用户，因此只保证同一用户启动的 CLI、TUI
和 runner 互斥；当前 tapeserver 部署均由 `ain` 运行。若未来允许多个 Unix 用户直接
访问同一磁带机，需要改为具备明确 group/ACL 策略的主机级 `/run/lock`，或由设备
broker 统一持有设备，不能为每个用户各自建立互不相见的 lease。

# 34. 暂时不决定的架构问题

以下问题当前明确保持开放。

不要为了让架构文档看起来完整而提前决定。

### 设备控制

* `/dev/nstX` 数据传输与 `/dev/sgX` 控制如何在长时间写入中统一协调；
* 写入 workflow 如何接入现有 command queue；
* 是否需要比单 worker 更细的、但仍保持单一设备所有权的执行模型。

### 并发

* sync / async；
* thread 数量；
* telemetry polling 与数据传输如何协调；
* host I/O pipeline 如何实现。

### Buffering

* 512 MiB 默认 buffer 是否应按主机内存或磁带代际调整；
* 是否需要多级 preload 或多 source reader；
* 不同 NFS/CIFS latency 下的 block 和 buffer 调优策略。

### Hash

* hash 是否需要移入异步 pipeline；
* 是否支持 SHA-256 之外的算法。

### LTFS

* index 更新频率；
* writer 内部 class 划分；
* extent allocation 具体结构；
* cache 策略。

### TUI

* Milestone 13 独立完成页的具体布局；
* throughput graph 的 scale/zoom 交互是否需要开放给用户；
* 长时间操作的取消确认与无法立即取消时的状态表达。

---

# 35. 修改本架构文档的原则

`ARCHITECTURE.md` 不是永久不变的规范。

它记录当前已经确认的架构决定。

如果实现过程中发现：

* Linux tape API 的实际行为与假设不同；
* LTO 驱动器存在新的状态限制；
* LTFSCopyGUI 揭示了此前不知道的重要机制；
* LTFS 规范要求改变结构；
* 第一版垂直切片证明某项设计不可行；

应该修改本文档。

但是修改架构决定时，应明确记录为什么改变，而不是让代码静默偏离文档。

---

# 36. 当前最重要的原则

在当前阶段，优先级依次为：

```text
正确理解真实需求
        ↓
跑通完整垂直工作流
        ↓
理解真实磁带机行为
        ↓
根据实际经验确定内部架构
        ↓
再进行抽象和优化
```

不要反过来：

```text
提前设计复杂抽象
        ↓
实现大量底层组件
        ↓
最后尝试把它们拼成用户工作流
```

tapecpy 本次重新开始的核心目标之一，就是避免再次重复这种开发方式。
