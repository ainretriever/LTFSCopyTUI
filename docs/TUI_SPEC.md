# tapecpy TUI 规格

版本：0.1
状态：Milestone 12 / 13 / 14 已实现；Milestone 15 多源 Write 与阶段 1 验收已完成

## 1. 文档目的

本文档定义 tapecpy TUI 的信息架构、状态语义、主要页面、Telemetry 显示方式以及长时间磁带操作的交互原则。

本文档的主要目的不是规定每一个 widget 的具体实现，而是提前确定：

* TUI 必须展示哪些信息；
* 哪些信息属于核心状态，而不是界面自行推导；
* 磁带介质生命周期如何表达；
* Telemetry 如何采样和显示；
* 长时间操作如何保持可观察；
* 网络文件系统如何作为主要数据源和目标；
* Milestone 12 应验证哪些基础架构；
* Milestone 13 完整写入界面需要哪些状态。

TUI 使用：

* `ratatui`
* `crossterm`

CLI 与 TUI 必须继续调用同一套 Application API。

不得为了实现 TUI 而复制另一套磁带设备或 LTFS 操作逻辑。

---

# 2. TUI 设计目标

tapecpy TUI 的主要目标不是把 CLI 命令做成菜单，而是为长时间、状态丰富的磁带操作提供持续可观察的工作环境。

设计原则：

1. 当前操作的磁带机身份始终明确；
2. 当前物理介质状态始终明确；
3. 当前 LTFS volume 身份始终明确；
4. 长时间操作必须明确显示当前 phase；
5. 磁带速度和介质错误状态是重点信息；
6. 不隐藏重要的设备异常；
7. 不把不同原因导致的 unavailable、unknown 和 error 混成一个 `N/A`；
8. 破坏性操作不能由单个快捷键直接执行；
9. 界面刷新不能意味着每次 redraw 都访问磁带机；
10. TUI 只是 Application 状态的表示层。

---

# 3. 推荐终端尺寸

第一版 TUI 不以兼容极小终端为主要目标。

建议：

```text
最低尺寸：100 × 30
推荐尺寸：120 × 40 或更大
```

如果终端尺寸低于最低要求，可以直接显示：

```text
Terminal too small

Minimum: 100 × 30
Current: 82 × 24
```

第一版不要求为了兼容 `80×24` 大幅压缩主要诊断界面。

---

# 4. 全局设备上下文

用户选择一台磁带机之后，这台磁带机成为当前 TUI session 的全局设备上下文。

不同页面不应各自重新选择设备。

主要结构：

```text
启动
 ↓
Device Selection
 ↓
当前磁带机会话
 ├── Overview
 ├── LTFS
 ├── Health
 └── 当前 Operation / Workflow
```

所有页面顶部应持续明确当前磁带机和磁带身份。

例如：

```text
tapecpy │ HPE Ultrium 6-SCSI │ HU123456 │ /dev/nst0 │ E62115L5 │ e621 Archive 15
```

当某些字段当前不可获得时，应根据真实状态显示，而不是伪造默认值。

---

# 5. 磁带介质生命周期

TUI 必须区分磁带物理生命周期中的三个稳定状态。

不能使用简单的：

```text
media_loaded: bool
```

来描述介质状态。

稳定状态至少包括：

```text
NO_MEDIA_DETECTED
PRESENT_UNTHREADED
LOADED_THREADED
```

具体 Rust enum 名称可以在实现时决定，但状态语义必须保留。

---

# 6. NO_MEDIA_DETECTED

含义：

> 当前磁带机没有报告可访问的 cartridge。

实际物理上可能存在一盘磁带放置在磁带机入口，但如果磁带尚未被磁带机机械机构接收，tapecpy 无法知道这一事实。

因此不得显示：

```text
Empty
```

因为软件无法证明磁带机入口物理上为空。

应显示：

```text
Media state    No media detected
```

这一状态下通常只能可靠显示磁带机本身的信息。

例如：

```text
Drive
──────────────────────────
Model         HPE Ultrium 6-SCSI
Serial        HU123456
Tape device   /dev/nst0
SCSI device   /dev/sg4

Media
──────────────────────────
State         No media detected
```

不得假装存在：

* Barcode；
* MAM 信息；
* LTFS Volume Name；
* partition；
* LTFS generation；
* index；
* position。

---

# 7. PRESENT_UNTHREADED

含义：

> Cartridge 已经进入磁带机，并能够通过 MAM 等机制识别，但磁带尚未 thread 到完整磁带路径。

这一状态下通常可以读取 cartridge/MAM 信息，但尚不能访问实际磁带数据区域。

因此可以显示：

* 介质代际；
* Barcode；
* cartridge/MAM 信息；
* Write Protect；
* 其他无需绕带即可取得的介质属性。

但是不能将 LTFS 相关字段标记成：

```text
Unknown
```

更准确的语义是：

```text
Unavailable until threaded
```

示例：

```text
Drive
──────────────────────────
HPE Ultrium 6-SCSI
HU123456

Cartridge
──────────────────────────
State          Present / Unthreaded
Barcode        E62115L5
Generation     LTO-5
Write Protect  No

MAM
──────────────────────────
Volume ID      E62115
Medium Serial  1234567890
Remaining      1.42 TiB
Load Count     37
```

---

# 8. LOADED_THREADED

含义：

> Cartridge 已进入磁带机，磁带已经 thread，可以执行实际介质访问。

此时才能进一步取得：

* partition；
* logical position；
* LTFS Label；
* VCI；
* Index；
* LTFS generation；
* volume information；
* 文件数据；
* 其他依赖实际 tape access 的状态。

示例：

```text
Cartridge
──────────────────────────
State          Loaded / Threaded
Barcode        E62115L5
Generation     LTO-5
Write Protect  No

LTFS
──────────────────────────
Volume         e621 Archive 15
Generation     183
Index          OK
VCI            OK
Consistency    OK
Partition      b
Block          1842912
```

---

# 9. 状态可用性语义

Application API 和 TUI 不应把所有缺失字段都表示成无语义的 `None`。

至少应能够区分：

```text
不存在
当前状态下不可访问
该字段不适用
查询失败
数据已经过期
```

例如：

```text
Barcode
```

在 `NO_MEDIA_DETECTED` 下属于没有 cartridge，因此不存在可取得值。

而：

```text
LTFS Generation
```

在 `PRESENT_UNTHREADED` 下属于：

```text
Unavailable until threaded
```

不能和：

```text
LTFS generation query failed
```

混为同一种状态。

TUI 不应自行猜测这些语义。

---

# 10. Load / Unload 操作语义

TUI 不应把所有机械动作压缩成模糊的：

```text
Load
Unload
```

至少需要在概念上区分：

```text
Load Unthreaded
Full Load / Thread

Unthread
Eject
```

### Load Unthreaded

目标是让 cartridge 进入磁带机，但不完成完整 thread。

目的包括：

* 读取 MAM；
* 查看 Barcode；
* 查看介质信息；
* 不进行完整磁带加载。

底层对应 LTO `LOAD UNLOAD` action `0x09`。如果某台设备不支持该功能，操作结果和
随后刷新的介质状态必须明确报告，而不能只根据命令返回值假装转换成功。

该操作的合法起始状态是 `NO_MEDIA_DETECTED`，目标状态是
`PRESENT_UNTHREADED`。不得把目标状态误用为操作的前置条件。`Full Load / Thread`
可以从 `NO_MEDIA_DETECTED` 或 `PRESENT_UNTHREADED` 开始。

### Full Load / Thread

让磁带真正 thread，进入可进行介质访问的状态。

状态转换：

```text
PRESENT_UNTHREADED
        ↓
Full Load / Thread
        ↓
LOADED_THREADED
```

底层对应 `LOAD UNLOAD` action `0x01`。

### Unthread

从：

```text
LOADED_THREADED
```

回到：

```text
PRESENT_UNTHREADED
```

磁带退出完整磁带路径，但 cartridge 仍保持在磁带机内。

底层对应 `LOAD UNLOAD` action `0x0A`。

### Eject

最终让 cartridge 离开磁带机的可访问状态：

```text
PRESENT_UNTHREADED
        ↓
Eject
        ↓
NO_MEDIA_DETECTED
```

具体设备支持哪些 partial-load/partial-unload 行为，由设备能力和底层实现决定。

TUI 必须 capability-driven，不应假定所有 LTO 驱动器行为完全相同。

---

# 11. Device Selection 页面

启动 tapecpy 后首先显示磁带机选择页面。

示例：

```text
┌─ Tape Drives ──────────────────────────────────────────────────────┐
│                                                                   │
│ > HPE Ultrium 6-SCSI                                              │
│   Serial     HU123456                                             │
│   Tape       /dev/nst0                                            │
│   SCSI       /dev/sg4                                             │
│   Media      Loaded / Threaded                                    │
│                                                                   │
│   IBM ULTRIUM-HH7                                                  │
│   Serial     12345678                                             │
│   Tape       /dev/nst1                                            │
│   SCSI       /dev/sg5                                             │
│   Media      No media detected                                    │
│                                                                   │
└───────────────────────────────────────────────────────────────────┘

↑↓ Select    Enter Open    R Rescan    Q Quit
```

这一页面只处理物理磁带机。

不在设备选择页面执行 LTFS workflow。

按 `Enter Open` 后必须立即离开 Device Selection 并进入 Overview，不能等待 LTFS
label/index 读取完成。进入 Overview 时只在后台刷新基础设备、介质和健康状态；
LTFS 字段保持未读取，直到用户明确执行：

```text
[6] LTFS Operations…
```

该动作立即读取 LTFS partitions、label、index 和一致性信息，完成后进入第三层
LTFS Operations 页面。`R Basic refresh` 不隐含 LTFS 读取，并应清除可能因换带而
过期的 LTFS 快照。

---

# 12. Overview 页面

Overview 是进入磁带机 session 后的默认页面。

其目的不是展示所有细节，而是快速回答：

1. 当前是哪台磁带机；
2. 当前磁带处于什么机械状态；
3. 当前 cartridge 是什么；
4. 当前 LTFS 是否可访问和健康；
5. 是否存在明显警告。

Overview 必须根据介质生命周期动态展示信息。

不是所有状态都显示固定的完整布局。

---

# 13. Overview：无介质状态

示例：

```text
┌─ Drive ────────────────────────────────────────────────────────────┐
│ Model         HPE Ultrium 6-SCSI                                  │
│ Serial        HU123456                                            │
│ Tape device   /dev/nst0                                           │
│ SCSI device   /dev/sg4                                            │
└───────────────────────────────────────────────────────────────────┘

┌─ Media ────────────────────────────────────────────────────────────┐
│ State         No media detected                                   │
└───────────────────────────────────────────────────────────────────┘
```

无介质时仅 cartridge 数据变为不可用；`Health (cumulative)` 和
`Cartridge Operations` 的框架仍保留，操作按状态灰显，页面导航、刷新和退出提示也
不能随 cartridge 一起消失。

---

# 14. Overview：Present / Unthreaded

示例：

```text
┌─ Drive ────────────────────────┬─ Cartridge ───────────────────────┐
│ HPE Ultrium 6-SCSI             │ State          Present / Unthreaded│
│ Serial      HU123456           │ Barcode        E62115L5           │
│ /dev/nst0   /dev/sg4           │ Generation     LTO-5              │
│                                │ Write Protect  No                 │
└────────────────────────────────┴───────────────────────────────────┘

┌─ MAM Cartridge Data ───────────────────────────────────────────────┐
│ Volume Identifier  E62115                                         │
│ Medium Serial      1234567890                                     │
│ Remaining          1.42 TiB                                      │
│ Load Count         37                                            │
└───────────────────────────────────────────────────────────────────┘

右侧 Health 下方显示与 Loaded / Threaded 相同的 Cartridge Operations 框；当前不可用
的动作灰显。
```

---

# 15. Overview：Loaded / Threaded

示例：

```text
┌─ Drive ─────────────────────────┬─ Cartridge ──────────────────────┐
│ HPE Ultrium 6-SCSI              │ State          Loaded / Threaded │
│ Serial       HU123456           │ Barcode        E62115L5          │
│ /dev/nst0    /dev/sg4           │ Generation     LTO-5             │
│                                 │ Write Protect  No                │
└─────────────────────────────────┴──────────────────────────────────┘

┌─ MAM Cartridge Data ────────────┬─ Health (cumulative) ────────────┐
│ Volume ID    E62115             │ TapeAlert       None             │
│ Manufacturer IBM                │ Corrected W     +1823            │
│ Medium Serial 1234567890        │ Hard W          +0               │
│ Remaining    1.42 TiB           │ Corrected R     +0               │
│ Maximum      1.50 TiB           │ Hard R          +0               │
│ Load Count   37                 ├─ Cartridge Operations ───────────┤
│ Total Written 22.4 TiB          │ [1] Load Unthreaded  装入，不穿带│
│ Total Read   18.1 TiB           │ [2] Load & Thread    装入并穿带  │
│                                 │ [3] Unthread        退带，不弹出 │
│                                 │ [4] Eject           直接弹出     │
│                                 │ [5] Erase…          擦除…        │
│                                 │ [6] LTFS Operations… LTFS 操作… │
└─────────────────────────────────┴──────────────────────────────────┘
```

Overview 不显示 Volume Name、LTFS generation、index 或 consistency。这些字段只在
用户显式执行 `[6] LTFS Operations…` 后显示于 LTFS 页面，避免 Overview 的可用性
依赖绕带。

Overview 中的 corrected/hard error 默认优先显示当前 session delta。

原始累计计数器可以在 Health 页面显示。

Overview 不再保留独立的底部操作提示条。六项 cartridge 操作集中在 Health 下方，
按当前机械状态、写保护状态和 detached job 的设备所有权显示 `Ready`、不可用或
`Locked`。`[5] Erase…` 只进入已有的破坏性确认工作流；`[6] LTFS Operations…`
只进入 LTFS 页面，不自动读取 label/index。操作框底部同时保留 `F1`–`F4` 页面导航、
`R Refresh` 和 `Q Back/Exit` 提示；这些提示与 cartridge 操作一样逐项纵向排列，不能
压缩成一条容易截断的横向快捷键栏。

Overview 的 telemetry 状态只显示 `HH:MM:SS`，不显示日期、秒的小数部分
或时区后缀，避免周期刷新消息挤占操作框宽度。

顶部框只显示当前 drive/cartridge 身份和右对齐的 `Status`，不重复显示 `F2 LTFS`、
`F3 Health`、`F4 Jobs` 或 `D Details`。页面导航提示集中在 Overview 的操作框中。

`[6] LTFS Operations…` 是所有 LTFS 操作的唯一入口；`F2` 不得绕过 partition probe
直接进入页面。probe 找到有效卷时第三界面开放 Read/Write；找不到 LTFS partition
或有效 label 时第三界面显示明确错误、禁用 Read/Write，但必须保留 Format 入口。
第三界面集中排列 `[1] Read LTFS…`、`[2] Write LTFS…` 和 `[3] Format LTFS…`。

穿带、退带和弹出由单一 device worker 串行执行。命令进行中显示 `Working` 模态框，
禁用其他设备命令，并明确提示不要移除介质。SCSI 命令返回 GOOD 后仍要刷新 basic
snapshot，只有实际介质生命周期符合目标状态才向用户报告完成；这类长命令不使用
无法反映设备内部进度的伪进度条。

`[4] Eject` 不要求用户先执行 `Unthread`。无论 cartridge 是
`PRESENT_UNTHREADED` 还是 `LOADED_THREADED`，host 都解除门锁并直接发送 Eject
action；需要的退带动作由驱动器完成。特别禁止在 Eject 前无条件发送 REWIND，因为
未穿带介质会在 REWIND 阶段失败。

---

# 16. LTFS 页面

Milestone 12 的 LTFS 页面主要用于显示 volume 状态和一致性。

第一阶段不要求在这里实现完整文件浏览器。

示例：

```text
┌─ LTFS Volume ─────────────────────────────────────────────────────┐
│ Barcode             E62115L5                                     │
│ Volume Name         e621 Archive 15                              │
│ Generation          183                                          │
│ Index Partition     a                                            │
│ Data Partition      b                                            │
│ Current Partition   b                                            │
│ Logical Block       1842912                                      │
│                                                                  │
│ Index Status        OK                                           │
│ VCI Status          OK                                           │
│ Index / VCI         Consistent                                   │
│                                                                  │
│ Capacity            ...                                          │
└───────────────────────────────────────────────────────────────────┘
```

Milestone 11 已经实现的：

* generation；
* index/VCI consistency；
* write failure semantics；
* consistency diagnosis；

必须由 Application API 提供。

TUI 不重新实现一致性判断逻辑。

---

# 17. Health 页面

Health 页面是 tapecpy 的主要诊断页面之一。

主要内容：

1. 16 通道实时 Channel Error Rate；
2. TapeAlert；
3. corrected/hard read/write errors；
4. session delta；
5. session worst channel；
6. 其他设备/介质健康数据。

---

# 18. 16 通道 Channel Error Rate

Channel Error Rate 不绘制历史曲线。

使用固定的 4×4 实时矩阵。

示例：

```text
┌─ Channel Error Rate — log10(BER) ─────────────────────────────────┐
│                                                                   │
│ CH00  -6.12    CH01  -5.84    CH02  -6.31    CH03  -5.92          │
│ CH04  -6.08    CH05  -5.71    CH06  -6.20    CH07  -5.65          │
│ CH08  -5.96    CH09  -5.28    CH10  -5.77    CH11  -6.02          │
│ CH12  -6.41    CH13  -5.88    CH14  -6.17    CH15  -5.73          │
│                                                                   │
│ Worst now       CH09   -5.28                                      │
│ Session worst   CH09   -4.91                                      │
│ Updated         21:43:25                                          │
└───────────────────────────────────────────────────────────────────┘
```

规则：

* CH00～CH15 位置固定；
* 不按 BER 数值重新排序；
* 每 5 秒随 telemetry sample 更新；
* 当前最差通道突出显示；
* session worst 独立保存；
* `-inf` 原样显示；
* 不将 `-inf` 替换成任意人为数值；
* CCP 未前进时，如果核心兼容逻辑返回 `-2.98`，继续显示该数值；
* 这种状态应使用弱化显示并标明 `idle`；
* 查询失败时保留上一帧；
* 上一帧必须标记 `STALE`；
* 显示最后一次成功采样时间。

`Worst` 的定义为：

> `log10(BER)` 数值最大的通道最差。

例如：

```text
-5.28
```

比：

```text
-6.12
```

更差。

这一判断最好在核心层建立测试。

核心层可以继续保留 10 分钟 BER history 用于诊断或日志，但 TUI 不绘制 16 条 BER 历史曲线。

---

# 19. Health 其他信息

Channel BER 下方可以显示：

```text
┌─ Drive / Media Health ────────────────────────────────────────────┐
│ TapeAlert                   None                                  │
│ Corrected write errors      +1823                                 │
│ Hard write errors           +0                                    │
│ Corrected read errors       +0                                    │
│ Hard read errors            +0                                    │
│ Temperature                 ...                                   │
│ Cleaning required           ...                                   │
└───────────────────────────────────────────────────────────────────┘
```

字段是否存在由实际 drive capability 决定。

不支持的字段应明确表达“不支持”，而不是伪装成查询失败。

---

# 20. Telemetry freshness

Telemetry value 不应只有一个裸数值。

至少需要表达：

```text
value
timestamp
fresh / stale
last error
```

如果一次采样失败：

* 不立即清空上一帧；
* 保留最后成功值；
* 标记为 stale；
* 显示最后成功更新时间；
* 保留采样失败原因。

示例：

```text
Channel Error Rate [STALE]

Last successful update: 21:43:25
Refresh failed: LOG SENSE ...
```

TUI 不应自行维护不可追踪的“上一帧缓存”。

freshness 应尽可能成为 Application/Core telemetry 状态的一部分。

---

# 21. 刷新周期

必须区分：

```text
UI render rate
```

和：

```text
device telemetry sample rate
```

界面频繁 redraw 不意味着频繁向磁带机发送 SCSI command。

建议初始目标：

```text
UI / keyboard response     即时
Operation progress         250–500 ms
Tape throughput            约 1 s
Buffer occupancy           约 1 s
Position                   事件驱动或约 1 s
BER                        5 s
Error counters             5 s
TapeAlert                  5 s
Temperature                5 s
Barcode / cartridge info   状态变化时
LTFS consistency           index变化或明确刷新时
```

实际周期允许根据真实设备行为调整。

---

# 22. 写入速度

写入页面不显示平均速度。

平均吞吐量不是主要诊断指标。

重点展示：

```text
当前 Tape Write Throughput
+
最近一段时间的吞吐历史曲线
```

用户主要需要观察：

* 突然掉速；
* 周期性掉速；
* 长时间低速；
* 速度恢复；
* buffer starvation；
* 可能的 shoe-shining 类行为。

---

# 23. 吞吐历史图

吞吐历史曲线是 Write 页面最重要的性能可视化之一。

必须：

* 横跨整个 TUI 可用宽度；
* 使用明显大于普通 sparkline 的垂直空间；
* 默认使用 Braille 高分辨率绘制；
* 视觉效果接近 btop 的历史图；
* 不挤在半宽的 Performance panel 中。

建议使用 Ratatui：

```text
Chart
GraphType::Line
Marker::Braille
```

Braille 默认作为主要绘图方式。

可以以后提供较低分辨率 fallback，例如：

```text
HalfBlock
Block
```

第一版不需要支持所有 Ratatui marker。

---

# 24. Throughput history 时间语义

Core 保留固定时间范围的历史数据。

例如：

```text
1 second/sample
10 minute history
```

TUI 根据当前图表宽度进行重采样。

不能让：

```text
终端有多少列
```

直接决定：

```text
保存多少秒历史
```

终端 resize 后，历史时间窗口应保持稳定，只改变绘制采样密度。

---

# 25. 吞吐图 Y 轴

Y 轴应从 0 开始。

不要对最近几个 sample 做极端自动缩放，否则轻微波动会看起来像巨大性能变化。

自动 scale 应使用稳定的离散 ceiling，例如：

```text
100 MiB/s
150 MiB/s
200 MiB/s
250 MiB/s
300 MiB/s
400 MiB/s
...
```

scale 可以在明显超过当前 ceiling 后提升。

不要随每一个 sample 连续改变 Y 轴。

以后可以考虑允许用户手动固定 scale，但不是 Milestone 12/13 的必要条件。

---

# 26. 当前速度显示

当前实时 Tape Throughput 直接显示在图表标题或边缘。

例如：

```text
┌─ Tape Write Throughput ─────────────────────────────── 138 MiB/s ─┐
```

不要为了显示：

```text
Current
Average
```

单独占用一个大 panel。

平均速度不显示。

---

# 27. 写入页面初步布局

Milestone 13 的 Write 页面可以采用类似：

```text
┌─ E62115L5 │ e621 Archive 15 │ WRITING_DATA ──────────────────────┐

┌─ Current ─────────────────────────────────────────────────────────┐
│ /mnt/archive/e621/...                                             │
│ File     ...                                                      │
│ Total    ...                                                      │
│ Files    ...                                                      │
│ Partition b    Block ...                                          │
└───────────────────────────────────────────────────────────────────┘

┌─ Tape Write Throughput ─────────────────────────────── 138 MiB/s ─┐
│                                                                   │
│                    Full-width Braille graph                       │
│                                                                   │
└───────────────────────────────────────────────────────────────────┘
Source I/O 172 MiB/s │ Buffer 768 MiB / 1.0 GiB │ Source CIFS

┌─ Channel Error Rate — log10(BER) ─────────────────────────────────┐
│ CH00 ... CH15 ...                                                 │
└───────────────────────────────────────────────────────────────────┘

┌─ Health ───────────────────────────────────────────────────────────┐
│ Corrected W ... │ Hard W ... │ TapeAlert ... │ Session worst ...  │
└───────────────────────────────────────────────────────────────────┘

[C] Cancel    [D] Details
```

具体 panel 高度可以实现时调整，但：

**吞吐曲线必须保持全宽和重点展示。**

---

# 28. SMB / NFS 作为主要数据源与目标

tapecpy 的主要实际使用场景之一，是在已经挂载的：

* SMB/CIFS；
* NFS；

文件系统和磁带之间读写数据。

因此网络文件系统必须作为一等 source/destination 考虑。

tapecpy 第一阶段：

**不负责实现 SMB/NFS 协议，也不负责挂载网络共享。**

它只操作 Linux 已经挂载的文件系统路径。

例如：

```text
/mnt/archive
/mnt/nfs
```

但 TUI 和 Application 层应尽可能识别路径所在 filesystem/mount。

---

# 29. Source / Destination 信息

源或目标不应只显示一个 path。

例如网络数据源可以显示：

```text
Source
──────────────────────────────
Path          /mnt/archive/e621
Filesystem    cifs
Remote        //truenas/archive
Mount         /mnt/archive
```

NFS：

```text
Source
──────────────────────────────
Path          /mnt/nfs/furaffinity
Filesystem    nfs4
Remote        nas:/archive
Mount         /mnt/nfs
```

本地：

```text
Source
──────────────────────────────
Path          /data/archive
Filesystem    zfs
Mount         /data
```

Application 层可以通过 Linux mount 信息获得这些 metadata。

真正文件 I/O 仍然通过普通 filesystem API。

---

# 30. 网络数据源性能诊断

Tape throughput 与 source/destination throughput 必须区分。

对于：

```text
SMB / NFS
   ↓
read pipeline
   ↓
LTFS
   ↓
Tape
```

磁带掉速可能不是介质问题，而是网络文件系统无法及时提供数据。

因此 Write 页面除了 Tape Throughput 主图之外，应显示辅助指标：

```text
Source I/O
Buffer occupancy
Source filesystem type
```

例如：

```text
Tape Write      138 MiB/s
Source I/O      172 MiB/s
Buffer          768 MiB / 1.0 GiB
Source          CIFS
```

如果出现：

```text
Source I/O      55 MiB/s
Tape Write      53 MiB/s
Buffer          0 MiB
```

用户可以合理判断存在 source starvation。

---

# 31. Read / Restore 的对称诊断

未来从磁带恢复到 SMB/NFS 时同样需要区分：

```text
Tape Read Throughput
Destination I/O Throughput
Buffer occupancy
```

例如：

```text
Tape Read        150 MiB/s
Destination I/O   72 MiB/s
Buffer            approaching full
```

可以用于判断目标存储是否成为瓶颈。

---

# 32. 主吞吐图语义

无论源/目标是本地还是网络：

**主速度图始终表示 tape-side throughput。**

原因：

> tapecpy 是磁带工具。

Source/Destination throughput 是用于解释主曲线的辅助信息。

第一版不要求同时绘制第二条 network throughput 历史曲线。

---

# 33. 网络文件系统错误

网络数据源错误必须与磁带错误区分。

例如 SMB/NFS source 读取失败时，不应最终只显示：

```text
I/O error
```

至少应能够保留：

```text
Operation       Write LTFS
Current file    /mnt/archive/foo/bar
Filesystem      cifs
Mount source    //truenas/archive
OS error        ...
Tape phase      WRITING_DATA
Tape partition  ...
Tape block      ...
```

不得把：

```text
source filesystem failure
```

错误归类成：

```text
MEDIUM_ERROR
```

Milestone 11 已建立的 failure semantics 应继续作为这一设计的基础。

---

# 34. Source 选择器

Milestone 13 的 source selector 不应假定所有数据来自本地磁盘。

可以考虑按 mounted filesystem 展示：

```text
Select Source

Local
  /data               zfs
  /home               btrfs

Network
> /mnt/archive         cifs    //truenas/archive
  /mnt/backup          nfs4    nas:/backup

[P] Enter path
[Enter] Browse
```

具体 UI 可以后续调整。

重要原则是：

> Network mounts 应明确可见，而不是偶然被当成普通目录访问。

## 34.1 Read / Write 方向选择与路径浏览顺序

用户选择磁带机后，应先明确选择 `Read` 或 `Write`，然后进入该方向专用的路径
选择界面。不要在尚未选择操作方向时用同一个文件浏览器猜测用户意图。

Write 的顺序固定为：

```text
Device Selection
        ↓
选择 Write
        ↓
浏览 Linux 文件系统并选择 source roots
        ↓
后台扫描 source，建立稳定 write plan
        ↓
得到总文件数和总字节数，执行 LTFS capacity 检查
        ↓
选择 LTFS destination directory
        ↓
显示完整 operation plan 和冲突检查
        ↓
用户最终确认 Start
        ↓
创建 detached Write runner
```

Read 的顺序固定为：

```text
Device Selection
        ↓
选择 Read
        ↓
读取并验证当前 LTFS index
        ↓
浏览 index 目录树，选择需要恢复的文件/目录
        ↓
根据 index length 汇总总文件数和总字节数
        ↓
浏览 Linux 文件系统并选择 destination
        ↓
检查目标冲突、写权限和可用空间
        ↓
用户最终确认 Start
        ↓
创建 detached Read runner
```

Write 选择界面应提供：

```text
Source       Linux 已挂载文件系统中的一个或多个文件/目录
Destination  当前 LTFS volume 中的目标目录
```

Read 选择界面应提供：

```text
Source       当前 LTFS volume 中的一个或多个文件/目录
Destination  Linux 已挂载文件系统中的输出文件或目录
```

脱离式 Read 不允许以 stdout 作为目标，必须在启动前解析为明确的输出文件或目录。
选择目录时应在确认页展示最终生成的目标路径；目标已存在、不可写、空间信息不可得
等情况必须在创建 runner 前明确报告或要求用户选择冲突策略。

文件浏览、目录导航和 operation plan 预览仍属于前台 TUI。只有用户在确认页选择
`Start` 后，才认为 Read/Write 已经开始并创建 detached runner。用户在此前退出、
返回或取消，不应遗留后台进程。

## 34.2 Write source size 与 LTFS capacity

Write 的 source scan 结果是后续进度模型的基准，至少包含：

```text
selected roots
files total
directories total
payload bytes total
每个文件在扫描时观察到的 size
```

runner 打开文件后仍必须验证实际读取长度与 plan 一致；源文件在计划后发生变化应按
现有 write failure semantics 失败，不能静默改变总进度。

在选择 LTFS destination 和最终确认前，应将 `payload bytes total` 与当前 LTFS
可用容量比较：

```text
payload <= available × 90%       正常
payload >  available × 90%       高容量占用警告，要求显式确认
payload >  available             阻止启动
available unknown                显示 Unknown，不伪造百分比，并要求显式确认
```

警告必须同时显示原始数值和百分比，例如：

```text
Selected payload   2.31 TiB
LTFS available     2.50 TiB
Planned use        92.4%
Warning            Selected data exceeds 90% of available LTFS capacity
```

capacity 的来源和采样时间必须保留在 Application state 中。MAM remaining capacity
可以作为当前已实现的数据源，但界面不能把不可用、读取失败或可能过期的值显示成 0。
90% 阈值是风险警告，不是精确的 tape-full 预测；index 增长、record padding 和设备
报告粒度仍可能消耗额外空间。

---

# 35. 大型目录选择

Source selector 不应要求先递归扫描完整目录树，才能让用户选择数据源。

对于拥有大量文件的数据集，这会严重影响交互。

推荐流程：

```text
选择一个或多个 source root
        ↓
立即完成选择
        ↓
后台扫描
        ↓
建立 write plan
```

例如：

```text
Selected sources

/mnt/archive/part01
/mnt/archive/part02
/mnt/archive/manifest.json
```

而不是：

```text
递归扫描几百万文件
        ↓
才能开始选择
```

---

# 36. LTO Barcode 规范

tapecpy 使用标准 8 字符 LTO Barcode。

基本结构：

```text
┌──────── Volume Serial ────────┐┌ Media ID ┐
             6 chars                  2 chars
```

例如：

```text
E62115L5
```

其中：

```text
E62115
```

为六字符 Volume Serial。

```text
L5
```

为两字符 LTO Media ID。

---

# 37. Barcode 输入

用户主要编辑前六位 Volume Serial。

后两位 Media ID 应优先根据实际 cartridge 类型自动派生。

例如普通数据带：

```text
LTO-5    L5
LTO-6    L6
LTO-7    L7
LTO-8    L8
LTO-9    L9
```

不得简单假设所有未来代际都遵循：

```text
L + 十进制代数
```

Media ID 应按照实际 LTO 标准处理，包括：

* 普通 R/W；
* WORM；
* M8；
* 后续代际。

---

# 38. Barcode Format 界面

Format workflow 可以显示：

```text
Cartridge
────────────────────────────
Type           LTO-5 Data

LTFS
────────────────────────────
Volume Serial  [ E62115 ]
Media ID       [ L5 ]       LTO-5 Data

Barcode        E62115L5

Volume Name    [ e621 Archive 15 ]
```

通常：

```text
Volume Serial
```

由用户编辑。

```text
Media ID
```

根据实际 cartridge 自动确定。

如果需要允许特殊情况修改 Media ID，也必须明确提示风险。

---

# 39. Barcode 全流程可见

完整的 8 字符 Barcode 必须在以下阶段持续可见：

* Format；
* Overview；
* Write；
* Finalization；
* Verify；
* Eject；
* 完成页面。

程序最终显示：

```text
E62115L5
```

而不是：

```text
E62115
```

Eject 后仍应明显显示 Barcode，方便用户进行物理贴标。

---

# 40. Workflow phase

长操作必须有明确 phase。

例如完整写入流程：

```text
PREPARING
FORMATTING
SCANNING_SOURCE
WRITING_DATA
FINALIZING_INDEX
VERIFYING
UNTHREADING
EJECTING
COMPLETED
```

具体 enum 名称可以变化。

重要原则：

> 最后一份用户数据写完不代表整个任务已经完成。

`FINALIZING_INDEX` 必须明确可见。

---

# 41. Progress 模型

Write 页面至少应能显示：

```text
当前文件
当前文件进度
总数据进度
文件数量进度
已写字节
当前 partition
当前 logical block
```

例如：

```text
Current
──────────────────────────────────
/mnt/archive/e621/...

File     68.1 GiB / 120 GiB
Total    2.71 TiB / 4.32 TiB
Files    237 / 381

Partition  b
Block      1842912
```

---

# 42. Cancellation

长操作期间 TUI 必须保持响应。

Milestone 12 应为 cancellation token / cancellation request 留好通道。

正常取消不能简单等同于：

```text
kill worker thread
```

TUI 的职责是请求取消。

具体安全退出粒度由 Application/Core 决定。

界面应能够表示：

```text
Cancellation requested
Waiting for safe stop point
```

而不是假装用户按下取消后操作已经立即停止。

---

# 43. 错误展示层级

错误/状态至少区分三个等级。

### Status

普通状态：

```text
Media loaded
LTFS index loaded
Telemetry refreshed
```

放在状态栏。

### Warning

例如：

```text
TapeAlert: Cleaning requested
Index / VCI mismatch
Telemetry stale
Network source slower than tape
```

明显展示，但一般不阻塞整个界面。

### Error

真正导致操作失败的错误。

应提供简短摘要，并允许打开详细诊断。

---

# 44. Error Details

详细错误页面或弹窗应尽可能显示核心已经保留的信息：

```text
operation
workflow phase
current file
filesystem / mount source
SCSI command
sense key
ASC
ASCQ
raw sense
partition
logical block
TapeAlert
```

TUI 不重新解释或丢弃 Milestone 11 建立的错误语义。

---

# 45. 破坏性操作

以下操作不能由单个快捷键直接立即执行：

* erase；
* format；
* repartition；
* 其他会破坏现有介质内容的操作。

例如按：

```text
E
```

最多只能打开：

```text
Erase Workflow
```

随后必须明确显示当前磁带信息并要求确认。

确认界面至少应突出：

```text
Drive Model
Drive Serial
Barcode
Volume Name
Cartridge Type
Requested destructive operation
```

---

# 46. 键盘导航基本原则

第一版可以使用类似：

```text
↑ ↓ / j k    移动选择
Enter        确认 / 打开
Esc          返回
Tab          切换 focus
F1           Help
F2           LTFS
F3           Health
R            Refresh
Q            返回 / 空闲时退出
```

介质控制可以考虑：

```text
L    Full Load / Thread
U    Unthread
E    Eject
```

具体快捷键可以实现时调整。

但必须遵守：

> 快捷键不得绕过 destructive-operation confirmation。

---

# 47. TUI 与 Application API 的边界

TUI 不直接：

* 打开 `/dev/nstX`；
* 打开 `/dev/sgX`；
* 发送 SCSI CDB；
* 解析 LTFS Index；
* 推导 Index/VCI consistency；
* 计算设备错误语义；
* 维护设备状态所有权；
* 自行执行磁带定位。

TUI 负责：

```text
render
navigation
user input
operation request
state presentation
```

Application/Core 负责：

```text
device state
media lifecycle
LTFS state
workflow
telemetry
diagnosis
errors
cancellation semantics
```

---

# 48. 后台设备所有权

Milestone 12 应验证：

* TUI 主线程保持响应；
* 长时间设备操作在后台执行；
* 磁带设备仍由单一组件统一持有；
* TUI 和 telemetry 不会分别独立操作同一设备；
* Application state 能够安全传递到 TUI；
* cancellation request 有明确通道。

具体采用：

* thread；
* channel；
* async；
* command queue；

由现有架构和实际实现决定。

TUI 规格不强制某一种并发模型。

---

# 49. Milestone 12 范围

Milestone 12 目标：

> 建立 TUI 基础架构和只读设备工作流，并验证状态模型、后台设备所有权、事件传递和错误展示。

应包括：

* 引入 `ratatui`；
* 引入 `crossterm`；
* TUI application state；
* 页面导航；
* event loop；
* 后台设备工作机制；
* Device Selection；
* Overview；
* LTFS 状态页；
* Health 页；
* 三态介质生命周期；
* MAM 与 loaded-state 信息分级；
* Barcode；
* Volume Name；
* write protect；
* LTFS generation；
* Index/VCI consistency；
* diagnosis warning；
* TapeAlert；
* corrected/hard error counters；
* 16 channel BER panel 布局和状态模型；
* telemetry freshness/stale；
* load/unthread/eject 基础操作；
* structured error display；
* cancellation 通道预留；
* CLI 与 TUI 共用 Application API。

Milestone 12 不要求：

* 完整 Format workflow；
* 完整 Erase workflow；
* 完整文件选择；
* 完整 Write workflow；
* 真正的写入 throughput graph；
* Finalization workflow；
* Verify workflow。

但是 Milestone 12 的状态和事件模型必须能够支持这些后续功能。

---

# 50. Milestone 12 验收场景

在测试专用磁带和真实 LTO drive 上：

```text
启动 tapecpy TUI
        ↓
发现磁带机
        ↓
选择磁带机
        ↓
显示设备身份
        ↓
正确区分：
No media detected
Present / Unthreaded
Loaded / Threaded
        ↓
在可用状态下显示 MAM / Barcode
        ↓
Loaded 后显示 LTFS 信息
        ↓
显示 generation / VCI / consistency
        ↓
显示 Health / TapeAlert / error counters
        ↓
显示 16-channel BER
        ↓
支持基础机械状态转换
        ↓
整个过程中 TUI 保持响应
```

设备或 telemetry 查询失败时：

* UI 不崩溃；
* 保留已有有效信息；
* 正确标记 stale/error；
* 可以查看详细诊断。

---

# 51. Milestone 13 范围

Milestone 13 在 Milestone 12 基础上接入完整写入工作流。

包括：

* Source selector；
* SMB/NFS mount awareness；
* 后台 source scan；
* write plan；
* Format；
* Barcode；
* Volume Name；
* 写入 progress；
* 全宽 Braille Tape Throughput graph；
* Source I/O throughput；
* buffer occupancy；
* 16-channel BER 实时刷新；
* session worst；
* TapeAlert；
* error counter delta；
* finalization；
* optional read-back verify；
* cancellation；
* safe unthread/eject；
* 完成页面；
* eject 后 Barcode 提醒。

---

# 52. 暂未决定的问题

以下细节暂时不需要在开始 Milestone 12 前完全固定：

* 最终颜色方案；
* 各种 warning 对应什么颜色；
* Ratatui theme；
* 页面 tab 的最终快捷键；
* throughput graph 确切高度；
* exact UI render frequency；
* throughput history 最终保存 5 分钟还是 10 分钟；
* Braille fallback 的最终 marker；
* source selector 最终 widget；
* 是否提供鼠标支持；
* terminal resize 后各 panel 的精确布局；
* 是否提供用户可调 graph scale；
* compact layout。

实现时不要为了填补这些空白而扩大 Milestone 12 范围。

---

# 53. 当前最重要的 TUI 原则

TUI 的核心不是：

```text
把 CLI 做成漂亮菜单
```

而是：

```text
让用户持续知道：
我正在操作哪台磁带机
        ↓
里面是哪盘磁带
        ↓
磁带处于什么机械状态
        ↓
LTFS 当前处于什么状态
        ↓
数据现在写到哪里
        ↓
磁带速度是否正常
        ↓
16 个读写通道是否健康
        ↓
数据源是否能够持续喂饱磁带
        ↓
当前异常到底来自磁带、驱动器还是数据源
```

tapecpy TUI 应保持这一目标，不重新制造一个新的黑盒。
