# RAW / TAR 顺序写入工作流

状态：Milestone 16 第一版设计与实现基线（2026-08-15）。

## 1. 范围

RAW 提供不带 tapecpy 私有 header 的顺序磁带 I/O。第一版垂直切片为普通文件到
磁带的覆盖写入；TAR 随后作为 RAW 字节流上层的标准 archive codec 接入。RAW
不保存文件名、目录、权限或时间戳，TAR 不发展为第二套磁带文件系统。

## 2. LTFS MAM 覆盖判定

RAW/TAR 写入前读取所有当前可访问分区的 MAM，并得到三态结果：

* `LtfsDetected`：Application Name 以 `LTFS ` 开头、Application Format Version
  是 LTFS 版本，或 VCI 含有效 `LTFS\0` ACSI；必须额外确认覆盖 LTFS。
* `NoLtfsDetected`：MAM 查询成功且没有上述标记；不增加 LTFS 专用确认。
* `MamUnknown`：MAM 读取失败、只读到部分现有分区或属性损坏；不得静默视为空白，
  必须用独立的 unknown-MAM 风险确认。

Barcode、介质厂商和用户文本标签不单独构成 LTFS 证据。检测依据是 MAM 的应用格式
标记，不替代正常 LTFS label/index 识别。

CLI 第一版以显式参数表达确认：所有覆盖写均要求 `--force`；`LtfsDetected` 额外要求
`--overwrite-ltfs`，`MamUnknown` 额外要求 `--overwrite-unknown-mam`。TUI 后续使用独立
确认页表达相同状态机。

## 3. 覆盖准备

为避免旧 partition、filemark 或 EOD 干扰，RAW/TAR 覆盖写不是从当前磁带位置继续：

```text
读取 MAM 并完成确认
→ load 并显式定位 P0 BOT
→ FORMAT MEDIUM type 0 移除现有分区
→ rethread
→ variable block mode
→ rewind + short erase，在 BOT 建立新逻辑 EOD
→ 将 P0 Host MAM 标记改为 tapecpy RAW/TAR，并清除 LTFS VCI
→ 再次显式 rewind（不得假定 ERASE/WRITE ATTRIBUTE 后仍位于 BOT）
→ 从 P0 BOT 写入顺序 records
→ filemark + flush
```

short erase 是逻辑覆盖准备，不是安全物理销毁；旧数据仍可能通过专业恢复手段读取。
MAM 只在 destructive preparation 成功后改写，避免操作尚未开始就丢失 LTFS 警告。

## 4. RAW record 语义

第一版默认 record size 为 512 KiB，可由 CLI 指定，范围为 1..=1 MiB。普通短读必须
继续填充同一 record，只有输入 EOF 的最后一条 record 可以不足。一个输入对象结束后
写一个 filemark，再用 count=0 WRITE FILEMARK flush。

数据通路复用 512 MiB bounded source pipeline、buffer 回收、SHA-256 和单一
`TapeSession` 设备所有权。第一版 CLI 输出累计字节进度；实时吞吐、LOG SENSE、
TapeAlert 和 detached job telemetry 在接入任务模型时补齐。SHA-256 是操作结果，
不写入 RAW 数据流。

## 5. 失败和取消边界

FORMAT/erase 成功后即已破坏旧卷，后续失败不得声称可以安全重试旧格式。第一版 CLI
同步写入不可脱离；下一切片必须在接入 TUI 前扩展 detached job operation kind，并在
record 边界响应取消。任何 SG_IO 都不在线程间并发。

## 6. TAR codec 决策与第一版

第一版不手写 TAR header/encoder，而调用系统 GNU tar 作为纯流式 codec。GNU tar 只
读取 source 并向 stdout 生成 `--format=posix`（PAX）archive；它不打开磁带设备，也不
执行 erase、定位或 MAM 操作。tapecpy 独占 `TapeSession`，完成与 RAW 相同的 destructive
preparation，并用 512 MiB bounded pipe 把 archive stream 写成 tape records。这样既不
把设备状态交给子进程，又能直接以目标 reader 的实现生成互操作数据。

第一版 `tar-write` 接受一个文件、目录或 symlink。archive member 使用 source 的 basename，
GNU tar 在 source parent 下运行，因此不保存绝对路径。使用 `--sparse` 和 POSIX/PAX 格式；
一个 archive 对应一个 tape file，结束后由公共 RAW 通道写 filemark。任何 GNU tar 启动或
版本检查必须发生在破坏磁带前；encoder 在流中途失败时，任务必须明确报告磁带已经是
不完整的新卷。

`--verify` 同时验证读回 stream 的长度、SHA-256，并把读回字节流交给 `GNU tar --list`
解析；三者全部成功才报告 verified。读取/恢复切片仍必须拒绝绝对路径、`..` traversal
和越过 destination 的 symlink。

直接使用 GNU tar 与进程内 encoder 的差别主要在控制边界，而不在磁带格式：GNU tar
互操作最直接，且完整处理 PAX、长文件名、稀疏文件等细节，但引入 Linux host 工具依赖、
子进程生命周期和 stderr/退出码管理；进程内成熟 TAR 库更容易接入取消、细粒度进度和
恢复安全策略，但仍需 GNU tar 互操作测试。项目不自行发明或手写 TAR 格式实现。

## 7. 第一版验收矩阵

1. 纯逻辑：LTFS/no-LTFS/unknown MAM 分类和确认规则。
2. 纯逻辑：短读聚合为完整 record、末块允许不足、SHA-256 一致。
3. 真机：健康 LTFS 测试带未给 `--overwrite-ltfs` 时在任何破坏命令前拒绝。
4. 真机：确认后移除 LTFS 分区、short erase、写 RAW、filemark/flush 成功。
5. 真机：从 BOT 逐 record 读回，长度与 SHA-256 等于源文件。
6. 真机：操作后 MAM 不再分类为 LTFS，普通 RAW 再覆盖不需要 LTFS 专用确认。

## 8. 2026-08-15 真机结果

测试设备为 QUANTUM ULTRIUM 5（固件 3210），源文件位于 NFS。测试首先确认健康
LTFS generation 2 磁带在未提供 `--overwrite-ltfs` 时被拒绝，且拒绝后卷仍可正常
识别。初次确认覆盖暴露出 FORMAT MEDIUM type 0 在该驱动器上对当前位置敏感：LTFS
检查停留在 partition 1 时返回 sense `3B/0C`；在 FORMAT 前显式定位 P0 BOT 后成功。

首轮写入又证明不能假定 short ERASE 完成后仍处于 BOT：写入和读回字节数相同但
SHA-256 不同。在 MAM 更新后增加显式 rewind 后重新测试成功：

* 输入：744,737,034 bytes，NFS 普通文件；
* RAW 布局：512 KiB record，共 1,421 records，末 record 为短 record；
* 源与读回 SHA-256：
  `55d274ea02088a15bcf13eda32188be3b32e0227163c733d796489ace21b5b77`；
* `verified=true`，总耗时 5m32.894s（包含同步 short erase、写入和读回）；
* 写入后的 MAM 为 `tapecpy RAW` / `RAW`，VCI 已清零，partition 1 已移除；
* 第二次写入被分类为 `NoLtfsDetected`，不带 `--overwrite-ltfs` 即可直接覆盖。

上述位置要求是工作流约束，不应简化为某一驱动器的偶发现象。后续 TAR 通路也必须
复用同一 destructive preparation，而不是自行拼装 SCSI 命令序列。

## 9. 2026-08-15 TAR 第一版真机结果

测试使用 tapeserver 的 GNU tar 1.35 和 NFS 上包含中文、特殊字符、压缩文件及 CAD
大文件的目录。MAM 初始分类为 `NoLtfsDetected`，因此不要求 `--overwrite-ltfs`。

* GNU tar POSIX/PAX stream：837,416,960 bytes；
* RAW 布局：512 KiB record，共 1,598 records；
* 源 stream 与磁带读回 SHA-256：
  `bfb98186d44483e4c12b58e3d63108935fc72bc19acba57db097fca8b4d84133`；
* 读回 stream 已由 GNU tar 1.35 `--list --file=-` 完整解析；
* `verified=true`，总耗时 5m29.823s（包含同步 short erase、写入和一次读回）；
* 测试后 MAM 应显示 `tapecpy TAR` / `TAR`，介质保持单分区。

这项验收证明磁带上的字节流可由 GNU tar 读取；下一切片仍需实现面向用户的 TAR
恢复命令、安全路径策略，以及 detached job/TUI 集成。

## 10. RAW / TAR 恢复模型

恢复不能仅凭 MAM 决定格式。`tapecpy RAW`、`tapecpy TAR` 和 LTFS 标记只提供建议；
旧软件、失败写入或手工写带可能留下缺失或过期的 MAM。第一版不实现 tapecpy 私有
manifest，也不在磁带上执行耗时的 TAR list 或选择性恢复。

RAW 与 TAR 使用同一恢复通路：从 P0 BOT 顺序拼接 records，遇到第一个 filemark 停止，
输出单一字节文件。TAR 磁带由此无损恢复为 `.tar` 文件，磁带只需顺序读取一次；成员
浏览、选择和解包全部在完整镜像落盘后由 GNU tar 在磁盘上完成。

启动恢复前，目标文件系统的可用空间必须严格大于当前 LTO 代际的最大原生容量，而不
使用当前 MAM remaining capacity 或猜测本次 archive 大小。代际或容量上限无法可靠
识别时阻止启动。第一版输出文件必须不存在；失败后保留明确标记的部分输出，不得将其
报告为完整 archive。磁带读取与 destination writer 之间使用有界缓存，以减少 NFS/CIFS
写入延迟造成的 tape starvation。

这项空间规则只保证完整磁带镜像可以落盘。如果用户随后在同一个文件系统解包 TAR，
还必须另外为解包后的文件准备空间；最坏情况下 archive 与恢复内容会同时占用接近两份
磁带容量。

### 10.1 LTO-5 / CIFS 真机恢复

当前 `tapecpy TAR` 测试带恢复到 CIFS：目标文件系统可用 7,740,666,675,200 bytes，
通过 LTO-5 `>1,500,000,000,000` bytes 门槛。一次顺序读取恢复 837,416,960 bytes、
1,598 records，终止于 filemark；镜像 SHA-256 为
`bfb98186d44483e4c12b58e3d63108935fc72bc19acba57db097fca8b4d84133`，与写入阶段
一致。落盘镜像由 GNU tar 1.35 正常列出 177 个成员。磁带读取约数秒完成，CIFS
flush/sync 使总操作耗时 1m30.226s。

同一恢复请求指向仅有约 78.8 GB 可用空间的本地文件系统时，在创建输出文件和移动
磁带前被拒绝，证明容量门槛不会留下空文件或部分文件。
