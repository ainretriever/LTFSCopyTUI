# LTFS MAM 与 VCI 审阅记录（2026-08-10）

本文记录 tapecpy 的 LTFS MAM 数据模型、普通写入后的 Volume Coherency
Information（VCI）提交，以及与 LTFSCopyGUI、OpenLTFS 和 SNIA LTFS 2.4.0
规范的对照结果。

规范依据为 `docs/SNIA-LTFS-Format-2.4.0-TechPosition.pdf` 第 10 章
（Medium Auxiliary Memory，规范页 51–56）。

## 1. 分层

- `ltfs::mam`：纯数据层，定义 LTFS MAM identifier、Host attribute 编码、
  VCR 有效性检查和 VCI 编解码；
- `device::tape`：只负责 READ/WRITE ATTRIBUTE，提供逐项和整分区读取；
- `app`：决定提交顺序，即完成哪些 index、何时 flush、读取哪个 VCR、为哪些
  完整分区更新 VCI；
- CLI：`tapecpy mam [选择器]` 只读显示两个分区的 MAM，并解析 VCI。

MAM 数据不能代替 label/index。MAM 可能缺失、陈旧或不受支持，卷是否有效仍以
磁带上的 LTFS constructs 为准。

## 2. Host-type attributes

格式化时尝试写入：

| ID | 内容 | LTFS 2.4 支持级别 |
|---|---|---|
| 0800h | Application Vendor = `tapecpy` | Mandatory |
| 0801h | Application Name = `LTFS tapecpy` | Mandatory |
| 0802h | Application Version | Mandatory |
| 0803h | NUL 终止的 UTF-8 Volume Name | Optional |
| 0804h | Date and Time Last Written | Optional/设备属性 |
| 0805h | Text Localization Identifier = 81h | Optional |
| 0806h | Barcode | Optional |
| 080Bh | Application Format Version = `2.4.0` | Mandatory |
| 0820h | Volume UUID | Optional |

Mandatory attribute 写入失败会终止格式化。Optional attribute 写入失败会产生
可观察警告并继续；规范明确要求在 MAM 空间或能力不足时优先 Mandatory 项。

LTFSCopyGUI 使用 `OPEN`、`LTFSCopyGUI` 和 localization 00h。对照规范发现：

- 0801h 必须以 `LTFS ` 开头，因此 tapecpy 使用 `LTFS tapecpy`；
- 0803h 应 NUL 终止，而不是用空格填满 160 bytes；
- 0805h 推荐 81h（UTF-8）。

Quantum LTO-5 接受除 0820h 外的上述属性；写 0820h 返回 ILLEGAL REQUEST / 
ASC 26h。由于它是 Optional，格式化记录警告后正常完成。

## 3. VCI 数据

每个 VCI 080Ch 包含：

```text
VCR length | VCR | generation (u64 BE) | index block (u64 BE)
| ACSI length (u16 BE) | "LTFS\0" | 36-byte UUID | NUL | version 01h
```

Quantum LTO-5 的 VCR 0009h 为 4 bytes。为了与 LTFSCopyGUI、OpenLTFS 和
SSC 的实际 70-byte VCI 布局兼容，编码时将它作为大端数左侧补零为 8 bytes。
parser 按 VCR length 处理，不把 8 写死为解析前提。

拒绝为空、全零或全 FF 的 VCR，也拒绝 block 0 和无效 UUID。

## 4. 提交顺序

格式化和普通写入共享同一 `update_volume_coherency`：

1. data partition index 已完整写入并以 filemark 结束；
2. index partition 镜像已完整写入并以 filemark 结束；
3. WRITE FILEMARK count=0 flush；
4. 立即读取 VCR 0009h；
5. 为 index partition 写入包含其 index block 的 VCI；
6. 为 data partition 写入包含其 index block 的 VCI。

普通 WriteSession 新增 `UpdatingCoherency` 阶段。若两个 index 已完成但 VCI
更新失败，命令会明确报告这个“磁带内容已提交、MAM coherency 未提交”的状态，
不会显示普通成功。

## 5. 真实设备验证

设备：Quantum ULTRIUM 5，固件 3210，`/dev/sg1`，测试卷 E6008A。

新格式化 generation 1：

```text
VCR: 00002593
P0 VCI: gen 1, block 5, UUID 6b252613-bfaa-458b-90a4-85245f8243f8
P1 VCI: gen 1, block 5, UUID 6b252613-bfaa-458b-90a4-85245f8243f8
```

写入 25-byte `/mam-write.txt` 后：

```text
VCR: 00002599
P0 VCI: gen 2, block 5, UUID 6b252613-bfaa-458b-90a4-85245f8243f8
P1 VCI: gen 2, block 9, UUID 6b252613-bfaa-458b-90a4-85245f8243f8
```

OpenLTFS 2.4.8.4 首次挂载刚完成写入的 generation 2 卷时：

- 没有执行 full medium consistency check；
- 直接识别 `(a,5) -> (b,9)`；
- 识别 Application Name `LTFS tapecpy` 和 UTF-8 localization 81h；
- 经 OpenLTFS 读回文件的 SHA-256 与源文件一致。

这解决了此前普通 WriteSession 不更新 VCI、导致 OpenLTFS 每次首次挂载都扫描
两个分区的问题。

## 6. 半提交观察

一次测试中的远端进程在写完新 Host attributes、但尚未写入 label/index 和新
VCI 时终止。此时 MAM 显示新的 volume name/application 信息，VCI 却仍指向
旧卷 UUID/generation，而磁带上没有有效 LTFS label。

因此：

- mount/inspect 不得仅凭 Host attributes 宣称介质是 LTFS；
- VCI 只有在 VCR 当前、UUID 与 label/index 一致时才可作为快速路径；
- format 的 Host attributes 不是提交点，完整 index + flush + VCI 才构成正常
  完成状态。

## 7. 后续问题

- VCI 已支持解析和显示，但 volume mount 快速路径仍主要扫描磁带 constructs，
  尚未利用 VCI 优化定位；
- 需要为“两份 VCI 只有一份写成功”建立故障注入和恢复测试；
- Date and Time Last Written 是否应由普通 WriteSession 主动更新，需要结合
  SPC/SSC 属性定义和其他厂商行为继续确认；
- Volume Advisory Locking MAM 1623h 尚未实现。
