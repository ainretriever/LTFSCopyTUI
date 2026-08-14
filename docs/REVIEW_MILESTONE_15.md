# Milestone 15：多源写入与第一阶段验收

## 1. 范围

Milestone 15 补齐第一阶段“多个文件/目录树”的 source selection，并验证完整 LTFS
写入主线。TUI 使用 `Space` 勾选多个普通文件或目录，使用 `S` 冻结并扫描选择；
扫描、容量预检和最终进度使用所有 source root 的合计值。

多个 root 写入用户选定的同一 LTFS 父目录，各自保留 basename。以下情况在启动
detached runner 前拒绝：

* source root 重复或互相包含；
* 多个 root 具有相同 basename；
* 任一目标已经存在；
* 任一 NFS/CIFS mount identity 在 runner 启动时发生变化。

所有 root 必须由同一个 `WriteSession` 规划和提交，只更新一次 data/index 两份
index 和 MAM VCI generation。旧的单 source job JSON 没有 `source_roots` 字段时仍
按原协议运行。

## 2. 实机验收（2026-08-14）

环境：Quantum ULTRIUM 5（serial `HU1340YHGE`），测试带 `M14ERSL5`，源文件系统
为 NFSv4 `192.168.10.228:/fs/1000/nfs`。

通过 TUI 同时选择：

* `DJI_20240825114847_0012_D.MP4`：956,758,053 bytes；
* `DJI_20240825115205_0013_D.MP4`：744,737,034 bytes。

合计 2 files / 1,701,495,087 bytes。任务
`000000000000000018cba7f04ce3565a-00001209` 在 TUI 退出后继续运行；新 SSH session
确认 runner `PPID=1`、session leader 为自身、没有 controlling TTY。最终结果：

* 两个文件在同一次 transaction 中写入；
* LTFS generation 从 1 更新为 2；
* SHA-256 read-back verify：`Passed`；
* 自动 `Unload / Eject`：`Succeeded`；
* 重新装载后根目录同时包含两个文件，大小与源一致；
* 有界诊断：`Healthy`，normal write safe；
* P0/P1 index 与两份 MAM VCI 均为 generation 2、同一 volume UUID；
* P0 index 位于 block 5，P1 data index 位于 block 3254。

测试结束时磁带已重新装载，保持 generation-2 Healthy 状态，供后续工作使用。

## 3. 第一阶段结论与剩余项

正常主线已覆盖介质检查、Erase、Format/Barcode、NFS/CIFS-aware source browser、
多源扫描和容量预检、可脱离 Write、实时吞吐和通道错误率、index/VCI 提交、可选
完整校验、自动弹出及完成页信息保留。

阶段 1 的核心验收可以视为通过。仍不应把以下后续范围混入这一结论：完整 Read
目录恢复工作流、RAW/TAR、多卷 spanning、高级故障恢复，以及尚未安排充足时间的
全带 long erase 实机测试。
