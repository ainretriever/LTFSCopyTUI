# Milestone 11：写入故障语义与索引一致性测试矩阵

本文定义完整 LTFS write workflow 在提交、中断和损坏 index 条件下的测试范围。
目标不是在本阶段实现高级恢复，而是确保 tapecpy 能够：

1. 明确报告失败发生在哪个阶段；
2. 区分未提交、半提交、已提交但校验失败；
3. 不把不一致卷继续当成普通可写卷；
4. 保留足够的设备、位置、index、MAM 和 telemetry 证据；
5. 在真实故障带上安全地完成只读识别验收。

## 1. 安全边界

测试介质分为两盘，不能混用：

| 介质 | 允许操作 | 禁止操作 |
|---|---|---|
| 测试专用磁带 | read、write、format、short erase、故障注入 | 未安排的全带 long erase |
| 真实故障带 | 只读 inquiry、MAM、LOG SENSE、READ POSITION、LOCATE、READ 和只读 OpenLTFS 对照 | write、filemark、MAM 写入、format、erase、修复 |

真实故障带验收前应优先打开物理写保护开关，并记录 cartridge、驱动器和设备路径
身份。即使工具存在缺陷，驱动器也必须拒绝写命令。任何修复尝试属于未来独立任务，
不能作为本里程碑验收的一部分。

故障注入只能发生在 SCSI 命令之间的安全边界。不得通过在 SG_IO 执行中拔线、
断电或杀死内核 I/O 来模拟正常取消。

## 2. 写入提交状态

Milestone 11 的错误结果至少要能表达以下状态：

| 状态 | 已完成内容 | 对用户的含义 |
|---|---|---|
| C0 `NotStarted` | 仅检查和规划 | 卷未被本次任务修改 |
| C1 `DataIncomplete` | 写入了部分或全部数据，但没有完整的新 data index | 本次数据不可由已提交 index 正常引用；旧 generation 应仍可识别 |
| C2 `DataIndexOnly` | 新 data-partition index 已完整结束 | 新 generation 可能可从 data partition 发现，index partition 尚未同步 |
| C3 `IndexesWritten` | 两个新 index 均完整写入并 flush | 磁带 index 已提交，MAM VCI 可能仍旧 |
| C4 `CoherencyPartial` | 只有一个 partition 的 VCI 更新 | index 已提交，但 MAM coherency 分裂 |
| C5 `Committed` | 两个 index 和两个 VCI 均完成 | 写入已提交；后续 verify 失败不能回退为未提交 |

这里的“完整”必须以命令成功及结束 filemark/flush 为准，不能以“开始发送 XML”
为准。C2--C4 不等同于数据丢失，但普通 write workflow 应先拒绝继续写入并要求
诊断，避免覆盖唯一可用的 generation。

## 3. 每个失败结果的最低证据

除底层错误链外，失败结果应尽可能保存：

- workflow phase 和 commit state；
- 当前文件、已完成文件数、已确认写入字节数；
- 最后已知 partition 和 logical block；
- 新旧 generation、volume UUID；
- data/index 两份新 index 的目标位置及完成情况；
- 两个 partition 的 VCI 更新结果；
- SCSI command、Sense Key、ASC/ASCQ 和 raw sense（若设备提供）；
- 已取得的 telemetry history、会话最差通道错误率和警告；
- 明确的 `safe_to_retry` / `requires_diagnosis` 结论。

失败不得发送 `Completed` 事件。错误后的健康快照应尽力读取，但读取失败不能覆盖
原始错误。

## 4. 自动化测试矩阵

### 4.1 纯软件和模拟设备

| ID | 注入点/输入 | 期望状态 | 核心断言 |
|---|---|---|---|
| A01 | 非 LTFS、缺 label/index | C0 | 写命令为零；报告具体缺失项 |
| A02 | volume locked | C0 | 不切换 write-anywhere，不定位写入 |
| A03 | 目标已存在/父目录缺失 | C0 | 规划阶段拒绝，介质不变 |
| A04 | previous-generation location 缺失、错误 partition 或 block 0 | C0 | 不猜测 EOD，不写入 |
| A05 | data append LOCATE 后位置不符 | C0 | 保留期望/实际位置，拒绝 WRITE |
| A06 | 源文件打开失败 | C0 | 不写 data record |
| A07 | 源文件在规划后缩短/增长 | C1 | 报告计划/实际长度，不写新 index |
| A08 | 第一个 data WRITE 返回 EOD 不一致 | C1 | 保守视为介质可能已改变；使用专门错误分类，不继续 finalization |
| A09 | 中途 data WRITE 失败 | C1 | 字节数只计成功记录；不写 index |
| A10 | 周期 diagnostic 查询失败 | 不改变提交状态 | 写入继续；记录 telemetry warning，不伪造 sample |
| A11 | data 前置 filemark 失败 | C1 | 不发送 data index WRITE |
| A12 | data index XML WRITE 中途失败 | C1 | 不同步 index partition |
| A13 | data index 结束 filemark 失败 | C1 | 不把它判为完整 data index |
| A14 | index partition LOCATE/位置确认失败 | C2 | 不写 index partition |
| A15 | index partition 前置 filemark 失败 | C2 | 不写 index XML |
| A16 | index XML WRITE 中途失败 | C2 | 不更新任何 VCI |
| A17 | index 结束 filemark 或 flush 失败 | C2 | 不进入 VCI 提交 |
| A18 | 读取 VCR 失败/无效 | C3 | 明确“indexes written, coherency not updated” |
| A19 | 第一个 VCI 写入失败 | C3 | 第二个 VCI 不应掩盖原错；结果可诊断 |
| A20 | 第一个 VCI 成功、第二个失败 | C4 | 精确报告哪一 partition 已更新 |
| A21 | verify LOCATE/READ 失败 | C5 | 明确写入已提交、校验失败 |
| A22 | verify hash 不匹配 | C5 | 报告路径、expected/actual hash |
| A23 | final health 查询失败 | C5 | 成功提交不因非关键 telemetry 失败变成未提交；保留 warning |
| A24 | 在每个可取消安全点请求取消 | 依当时 C0--C5 | 不开始下一个高层步骤；报告取消而非成功 |
| A25 | observer/TUI 消费事件缓慢 | 不改变提交状态 | 不允许另开设备句柄或改变命令顺序 |

模拟设备必须记录命令序列，使测试能断言失败后没有执行危险后续命令。failpoint
应按语义步骤命名，而不是依赖“第 N 次 SCSI 调用”，以免实现调整让测试静默失效。

### 4.2 index、label 与 MAM 一致性输入

| ID | 构造状态 | 期望识别与写入策略 |
|---|---|---|
| I01 | 两分区同 UUID、generation、合法 chain，VCI 一致 | 正常可写 |
| I02 | index partition 新、data partition 旧 | 报告分歧；只读可浏览明确选中的 generation；拒绝普通写入 |
| I03 | data partition 新、index partition 旧 | 同 I02；不得仅因 partition 名称偏好旧副本 |
| I04 | 一份 index XML 截断，另一份完整 | 报告损坏副本及采用的完整副本；写入前要求一致性处理 |
| I05 | 两份 index 均无法解析 | 不识别为正常可写 LTFS；保留候选位置和解析错误 |
| I06 | label UUID 与 index UUID 不同 | 严重一致性错误，拒绝写入 |
| I07 | 两份 index UUID 不同 | 不自动合并或选择最高 generation 写入 |
| I08 | self location 与实际位置不同 | 报告实际/声明位置；拒绝普通写入 |
| I09 | previous location 指向不存在、错误 partition 或未来 block | chain 无效；拒绝写入 |
| I10 | generation 倒退、跳变或 chain 循环 | 报告 chain 异常；不得无限扫描 |
| I11 | VCI generation/block 陈旧，但两个 index 一致 | 报告 stale VCI；不把 MAM 当作磁带事实来源 |
| I12 | 两份 VCI 不一致 | 报告 partition 级差异；拒绝普通写入 |
| I13 | VCI UUID 与 label/index 不同 | 报告 stale/foreign VCI；拒绝普通写入 |
| I14 | VCI 缺失或设备不支持 | 以磁带 constructs 判断；明确能力缺失，不伪报一致 |
| I15 | index 目标位置发生 unrecovered READ error | 返回完整 SCSI 证据，不退化为“没有 LTFS” |
| I16 | data extent 越过 EOD/遇到 filemark | 浏览可显示 metadata；读取具体文件时精确失败 |

对存在多个可解析 generation 的情况，诊断结果必须列出候选及选择理由。最高数字
不一定就是可信 generation；UUID、self/previous location、实际记录边界和 VCI
必须共同参与一致性判断。

## 5. 测试专用磁带集成矩阵

每个破坏性用例从重新 format 的已知 generation 1 卷开始，写入小型确定性数据，
记录 index/VCI/位置基线。通过安全 failpoint 在命令边界停止，不通过任意 `kill -9`
猜测时机。

| ID | 场景 | 停止点 | 重新装载后的检查 |
|---|---|---|---|
| T01 | 正常写入 | C5 | tapecpy/OpenLTFS 均看到新文件、hash 和相同 generation |
| T02 | data 中断 | C1 | 旧 generation 仍可浏览；新文件不被误报已提交 |
| T03 | data index 完成后中断 | C2 | 能发现两分区 generation 分歧并拒绝下一次普通写入 |
| T04 | 两个 index 完成后中断 | C3 | 两 index 新、VCI 旧；诊断明确，不执行自动修复 |
| T05 | 仅 P0 VCI 更新后中断 | C4 | 精确显示 P0/P1 VCI 差异 |
| T06 | 提交后 verify 注入失败 | C5 | 新文件正常存在；任务结果为“已提交、校验失败” |
| T07 | 安全取消 | C1、C2、C3 各一例 | 状态、事件和重新装载诊断与对应 failpoint 一致 |
| T08 | 成功后 unload/reload | C5 | 状态持久，VCI VCR 与 index generation/位置一致 |

每例均采集：命令日志、退出码、阶段事件、commit state、READ POSITION、两份
index 摘要、两份 VCI、health/TapeAlert，以及 OpenLTFS 只读对照结果。用例间重新
format，避免上一个半提交状态污染下一个用例。

## 6. 真实故障带只读验收

真实故障带是最终验收样本，不用于首次调试。执行顺序：

1. 确认物理写保护，拍照/记录外部标签；
2. 记录驱动器 vendor/model/serial、设备节点和介质装载状态；
3. 读取 health/TapeAlert 基线和两个 partition 的 MAM/VCI；
4. 执行 tapecpy 只读 volume/index candidate 诊断，保存每个候选的实际位置、
   XML 解析结果、UUID、generation、self/previous location；
5. 若能够安全列目录，只读 `ls` 并抽样读取明确可恢复的小文件；
6. 使用强制只读的 OpenLTFS 或厂商工具作对照，保存日志；
7. 再次读取 health/TapeAlert，安全 unload；
8. 核对前后 MAM/VCI 原始值完全未变化。

验收通过不要求修复磁带。通过标准是：程序不崩溃、不挂死、不写介质、不把
损坏状态误报为正常可写，能够指出故障所在 partition/位置和一致性关系，并保存
足以让后续恢复功能使用的证据。若物理读错误使部分信息不可取得，也必须保留
SCSI sense，并把“不可读取”与“不存在”区分开。

## 7. 实施顺序与提交门槛

实现顺序固定为：

1. 结构化 `WriteFailure`、commit state 和失败事件；
2. 可记录命令序列的设备接口及语义 failpoint；
3. A01--A25 自动化测试；
4. index/VCI 一致性诊断及 I01--I16 fixture；
5. 测试专用磁带 T01--T08；
6. 用户提供的真实故障带只读验收；
7. rustfmt、clippy、全部测试及文档复核；
8. 最后才提交 Milestone 11。

提交前必须满足：自动化测试全绿；真实测试卷可由 OpenLTFS 交叉验证；所有失败
路径都没有 `Completed` 事件；C2--C4 卷不能进入普通写入；真实故障带验收过程
没有介质写入。故障带的原始日志不得直接提交进仓库，先检查 serial、barcode、
文件名和其他可能敏感的信息，只提交去标识化结论。

## 8. 尚未解决但不能隐式决定的问题

- 正常 write 在发现 C2--C4 后是否提供“仅同步 index/VCI”的显式恢复命令；
- cancellation token 的 API、当前 record 完成后的最小取消粒度；
- index candidate 扫描上限和坏块重试 policy；
- OpenLTFS 对照是否可能在特定版本/参数下更新 MAM，验收前必须确认只读行为；
- 真实故障带是否还有物理介质故障，可能需要限制反复定位次数。

这些问题不影响先实现诊断和拒绝危险写入，但在提供任何自动修复之前必须明确。

测试专用磁带 T01--T08 和真实故障带只读验收均已完成，结果和测试中修复的问题
记录在 `docs/REVIEW_FAILURE_WORKFLOW.md`。
