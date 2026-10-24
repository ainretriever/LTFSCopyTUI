# Quantum LTO-5 实机测试准备（2026-10-22）

## 已完成的软件验证

P15：extent byteoffset 从 XML 到冻结计划、stdout/恢复/verify 全链路保留；共享 record
缓存、跨块、跨分区和 SHA-256 模拟测试通过。尚未增加 packed writer。
P18：块大小和 READ/WRITE(6) 24-bit transfer length 越界拒绝。
P12：完整 plan 只保留于 spec.json；state/IPC 为兼容 v1 的摘要；计划通过 Arc 共享；
终态缓存按 mtime/长度失效，旧状态/旧 IPC 超限回退兼容。

完整测试 168 passed、2 ignored；格式检查、Clippy -D warnings 和 release build 通过。

## 测试机与部署

* SSH：`ain@192.168.10.125`，hostname 为 `tapeserver`；本轮使用 IP 直连。
* Quantum ULTRIUM 5，固件 3210，serial `HU1340YHGE`。
* 数据设备 `/dev/nst0`，SCSI 设备 `/dev/sg0`；新 SSH 会话的 ain 已属于 tape 组。
* Ubuntu 26.04.1，x86_64，glibc 2.43，i3-12100TE。
* 系统盘初始约 214 GiB 可用；`/tmp` 为约 3.3 GiB tmpfs，不用于磁盘基线。
* 测试程序：`/home/ain/tapecpy-perf-20261022/bin/tapecpy`，与本机 release 二进制
  SHA-256 一致：`de04508f4ff1a3c4c33a109062f7007793ce9197525242a1eaced9b4c465c830`。

程序放在独立测试目录，没有覆盖系统安装。用户明确授权当前盘带可擦除、格式化和覆盖。
初次检查时未穿带，LOAD 后密度 0x58（LTO-5）、未写保护；旧 MAM barcode 为
A00393L7，但无有效 LTFS label。初始健康查询无活动 TapeAlert、uncorrected 为零。

## 格式化验收结果

已按用户对当前盘带的授权完成 LTFS 格式化：

* ANSI barcode / volume serial：`PERF01`，介质实际为 LTO-5。
* Volume Name：`Performance Test 2026-10-22`。
* UUID：`20ec029b-3788-4d3c-911a-8a4d876f28df`，generation 1。
* LTFS 2.4.0，512 KiB block，compression enabled，index p0/data p1。
* p0b5 / p1b5 均成功读取 generation-1 index；两份 VCI 均为 generation 1、block 5，
  VCR `00000000000003D5`。
* 独立有界 diagnose：`Healthy`、`safe_for_normal_write=true`。
* 可选 MAM attribute 0x0820 被驱动器拒绝，formatter 已明确报告并继续；必需的
  label、index 和 VCI 均由后续只读检查确认，不依赖 format 命令单独返回成功。

以上为测试准备阶段记录。后续已完成 32 GiB 与目录数据集测试，结果见 `HARDWARE_TEST_RESULTS_2026-10-22.md`；换带对照见 `HARDWARE_COMPARISON_2026-10-23.md`。

## 本地测试数据

已在测试机生成真实分配的不可压缩随机数据：

`/home/ain/tapecpy-perf-20261022/source-32g`

共 10,124 个文件、34,524,958,721 bytes；生成耗时 76.487 s。

* `large/stream.bin`：32 GiB，供稳定吞吐与恢复测试。
* `small/`：10,000 × 4 KiB，按每 1,000 个文件分目录。
* `mixed/`：116 个 1 MiB 文件，含中文路径；另有空文件和 1、4095、524287、524288、
  524289、1048577 字节的边界文件。
* `manifest.jsonl`：每个 payload 文件的相对路径、长度和 SHA-256。

生成器在 `scripts/prepare_perf_data.py`；拒绝覆盖现有数据目录，不使用稀疏文件或
全零可压缩内容。manifest 本身不计入上述文件数/容量。

## 后续测试顺序

1. 确认干净 PERF01 LTFS 卷，两个 index 与两份 VCI 一致，再记录 health baseline。
2. 本地磁盘大文件：detached write → index/VCI 提交 → 可选 read-back verify；
   保存 job state 和吞吐历史。读取恢复到全新路径后，与独立 manifest 比对 SHA-256。
3. mixed/ 和 small/ 目录：验证空文件、边界、中文路径、detached/attach，以及读恢复
   的文件列表、长度和完整 SHA-256。记录启动、稳态、最终化时间，分开分析。
4. 大计划：通过 TUI 目录恢复构建 10k 文件计划，检查 spec/state/IPC 大小与 Jobs 页
   可见性。普通 CLI start-read 是单文件入口，不能把它当成目录恢复接口。
5. 用户提供远端 NFS/SMB 路径后，使用同一数据集和独立恢复目录重复；记录挂载身份、
   参数、源/目标 host-only 吞吐和元数据延迟。不能用生成速度代替 host-only 读基线。
6. P15 真机互操作：另用独立 LTFS 实现生成非零 byteoffset 卷，验证读取、索引追加
   保留偏移及跨实现 SHA-256。自写零偏移卷不能证明 packed extent 互操作。

只在没有活动 job/设备 lease 时运行 health/volume/diagnose，避免额外诊断命令干扰
数据流。当前尚无命令间 gap/backhitch 计数埋点，吞吐历史不能独自证明“没有停带”。

localhost NFS 可以验证 NFS 客户端/服务端语义和操作成本，但不包含物理网络链路和
独立服务端竞争，页缓存也影响结果，不能代替远端 NFS/SMB 验收。
