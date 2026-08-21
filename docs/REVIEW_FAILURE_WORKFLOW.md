# Milestone 11 写入故障语义与测试带验证记录（2026-08-10）

本文记录 `docs/MILESTONE_11_TEST_MATRIX.md` 的实现和测试专用磁带验证结果。
设备序列号、完整卷 UUID 等现场标识不写入仓库。

## 1. 实现结果

Application 层新增结构化 `WriteFailure`，包含失败 phase、`WriteCommitState`、
文件/字节进度、最后可用位置、重试/诊断结论和 cancellation 标记。失败通过
`WriteEvent::failure` 发送，且不会发送 `Completed`。

提交状态为：

```text
NotStarted
DataIncomplete
DataIndexOnly
IndexesWritten
CoherencyPartial
Committed
```

公开的 `CancellationToken` 可由未来 TUI 从其他线程请求取消。writer 只在 record
完成或 index/VCI 阶段边界观察 token，不中断正在执行的 SG_IO。测试 CLI 的
`--cancelpoint` 和 `--failpoint` 只用于确定性集成测试，不属于普通归档选项。

写入前增加 VCI coherency 检查；data partition 引用位置之后若有未索引数据或
更新 generation，仍由安全 append 检查拒绝。C2--C4 以及 orphan tail 均不能
继续普通写入。

新增只读 `tapecpy diagnose`：分别扫描两个 partition，列出有效和损坏的 index
候选、实际位置、声明位置、generation、UUID 和两份 VCI，并分类：

```text
Healthy / MamUnavailable / NoUsableIndex / IndexCopyMissing
DivergentIndexes / ForeignIndex / InvalidIndexLocation
StaleVci / DivergentVci / DivergentLabels / UnindexedTail
```

扫描 data partition 时只累计看起来像 XML 的 record group。普通文件数据只统计
长度，避免诊断大卷时把整盘内容缓存到内存。

## 2. 自动化检查

- rustfmt：通过；
- clippy `--all-targets -- -D warnings`：通过；
- unit/doc tests：77 passed；
- `git diff --check`：通过。

纯逻辑测试覆盖提交边界、C4、取消 token、健康/分裂/stale/missing/foreign/
mislocated index 和 VCI 分类。真实 SCSI 命令失败仍由设备层已有 sense 测试与真机
返回覆盖；本里程碑没有建立一套与 `TapeSession` 平行的完整虚拟磁带实现。

## 3. 测试专用磁带 T01--T08

每个主要场景均从 tapecpy 重新 format 的 generation 1 卷开始。未执行全带 long
erase。

| 用例 | 注入结果 | 重新扫描结果 | 后续普通写入 |
|---|---|---|---|
| T01 正常提交 | C5 | `Healthy`，两份 gen 2 index/VCI 一致 | 允许 |
| T02 首 record 后停止 | C1 | `UnindexedTail`，旧 gen 1 仍完整 | 拒绝 |
| T03 data index 后停止 | C2 | `DivergentIndexes`（P0 gen 1 / P1 gen 2） | 拒绝 |
| T04 两 index flush 后停止 | C3 | `StaleVci`（index gen 2 / VCI gen 1） | 拒绝 |
| T05 第一份 VCI 后停止 | C4 | `DivergentVci` | 拒绝 |
| T06 verify 前停止 | C5 | `Healthy`，文件可完整读回 | 允许 |
| T07 三个取消边界 | C1/C2/C3 | `UnindexedTail` / `DivergentIndexes` / `StaleVci` | 均不报成功 |
| T08 unload/reload | C5 | 重载后仍为 `Healthy` | 允许 |

T02 首次运行发现进度事件未包含刚完成 record 的字节数，已修复并通过 T07/C1
复测。T05 首次运行发现 C4 内部标记被外层错误上下文包裹，已修复并再次在真机
确认 `CoherencyPartial`。

T08 写入 4 MiB 确定性数据，unload/load 后保持 generation 2。OpenLTFS 2.4.8.4
以只读方式直接识别 `(a,5) -> (b,16)`；读回 SHA-256 与 tapecpy 写入摘要一致，
SHA-256 和三个伪随机测试 xattr 均可见。卸载 OpenLTFS 后最终诊断仍为 `Healthy`，
LOG SENSE corrected/hard read-write error 为 0，TapeAlert 无活动 flag。

为复测最终 C4 报告，测试带最后停留在 `DivergentVci` 状态并已弹出。这是有意的
测试产物，不是待修复的用户介质。

## 4. 真实故障带只读验收

用户提供的真实故障带已在物理写保护下完成验收。未执行 format、erase、write、
filemark、MAM 更新或恢复命令，验收后已安全弹出。

该卷由 LTFSCopyGUI 3.5.4 创建，LTFS 2.4.0，最新 generation 24。诊断结果：

```text
P0 actual index: generation 24, block 5
P0 VCI:          generation 24, block 4
P1 actual index: generation 24, block 2505721
P1 VCI:          generation 24, block 2505721
UUID:            两份 label/index/VCI 一致
classification:  StaleVci / safe_for_normal_write=false
```

P0 block 4 是 index 前的 filemark。P0 VCI 因 off-by-one 指向 filemark，导致按
VCI 直接读取 index 时得到空记录而无法解析；真正有效的 P0 index 位于 block 5。
P0/P1 的 generation 24 index XML 均可解析，data partition index chain 仍完整。

OpenLTFS 2.4.8.4 只读挂载复现了这一过程：先从 P0 VCI 目标读取并报告 XML parse
failure，随后回退到 P1 generation 24 index，成功挂载并可访问根目录。物理写保护
被 OpenLTFS 和内核同时确认。

2026-08-20 的安全审计修复把同类 fallback 加入 tapecpy 普通只读卷识别：index
partition 当前副本的 XML 损坏或元数据不匹配时，按 data partition VCI 定点读取当前
generation。fallback 必须同时匹配 label UUID、VCI generation/block、实际物理位置和
index self-location；成功后仍报告 `IndexCopyMissing` 并禁止普通写入。若 index 分区
顺序扫描在最后一个有效 index 后发现损坏或未以 filemark 结束的记录组，也不会静默
采用旧 generation。该有界恢复不扫描完整 data partition，VCI 不可信时仍要求显式
`diagnose --full` 或后续恢复流程。

验收前后 MAM VCR、两份 VCI generation/block/UUID 完全不变。LOG SENSE
corrected/hard read-write error 均为 0，活动 TapeAlert page 为空。MAM 基本属性中的
历史 TapeAlert bitmap 为 `0x4000`，与当前活动 TapeAlert 是不同时间域，未把它
误报为本次读取错误。

为避免接近满容量的 data partition 被默认全盘扫描，`diagnose` 改为有界模式：
完整扫描 index partition，再按 VCI 和 index chain 定点读取 data index。只有显式
`diagnose --full` 才从 data partition block 4 扫到 EOD；本次故障带未执行 full。
