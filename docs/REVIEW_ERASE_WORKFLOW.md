# 磁带擦除工作流审阅记录（2026-08-09）

本文记录 tapecpy 三种擦除行为的设备命令、LTFSCopyGUI/OpenLTFS 对照结论、
真实设备验证结果和未完成测试。

## 1. 对外行为

CLI 提供三种明确、互斥的破坏性操作：

```text
tapecpy erase short [选择器] --force
tapecpy erase long [选择器] --force
tapecpy erase minimum [选择器] --force
```

三种模式都必须显式给出 `--force`。缺少确认参数时，程序会在发现或打开设备
之前拒绝执行。

### short

在当前介质 BOT 发送 ERASE(6)，LONG=0。它快速建立新的逻辑数据结束位置，
使旧内容不能按正常文件/index 路径访问，但不逐段重写整盘介质，也不承诺清除
所有残留 label/MAM。

### long

先用 FORMAT MEDIUM type 0 移除现有分区并重新穿带，再对未分区的完整介质
执行 ERASE(6)，LONG=1、IMMED=1。程序每 30 秒用 REQUEST SENSE 查询进度，
因此不会让整个操作受单条 SG_IO 1800 秒 timeout 限制。

### minimum

先创建最小 P0 和占用剩余容量的 P1，在最小 P0 上执行 long erase，最后移除
临时分区并恢复为未分区介质。该模式的目的不是安全清除整盘数据，而是用较低
时间成本运行少量 wrap，对二手介质进行有限的机械运行和写带检查。

## 2. 参考实现结论

优先对照了 `references/LTFSCopyGUI/LTFSCopyGUI/LTFSConfigurator.vb`：

- Quick Erase 直接发送 `{19h, 00h, 00h, 00h, 00h, 00h}`；
- Partial Erase 读取 Medium Partition mode page 0x11；
- MODE SELECT 把 P0 设为最小值 1、P1 设为 `FFFFh`（剩余容量）；
- FORMAT MEDIUM type 1 应用临时分区；
- 用 LOAD UNLOAD action `0Ah`/`01h` 重新穿带；
- 在 P0 发送 `{19h, 01h, 00h, 00h, 00h, 00h}` long erase；
- 完成后再次重新穿带并用 FORMAT MEDIUM type 0 移除分区。

tapecpy 沿用了以上设备序列，但没有复制 GUI 线程和全局状态。全部状态性命令
都由一个 `TapeSession` 持有，`EraseSession` 只负责应用层阶段编排与事件。

长擦除状态处理还对照了 OpenLTFS 2.4.8.4 的 Linux sg backend：它发送
LONG|IMMED（CDB byte 1 = 03h），随后 REQUEST SENSE 查询 00/16、00/18 和
16-bit progress indication。tapecpy 采用这个模型，以支持可能持续数小时的
全带 long erase。

## 3. 失败与恢复语义

最小分区模式在临时分区创建成功后，无论擦除是否成功，都会尝试重新穿带并
执行 FORMAT MEDIUM type 0。若擦除和恢复同时失败，错误信息会同时保留两项
原因，避免把介质仍处于临时分区状态隐藏起来。

FORMAT、LOAD UNLOAD、ERASE、REQUEST SENSE 的 SCSI/host/driver status 都必须
成功才算命令成功。三种操作独占设备，运行期间不得并发执行 media、volume、
format 或其他定位命令。

## 4. Quantum LTO-5 真实测试

设备：Quantum ULTRIUM 5，固件 3210，`/dev/sg1`；测试介质 `E6008A`。

### short erase

- 原介质为 tapecpy 格式化并写入过文件的 LTFS generation 2；
- short erase 在 7 秒内完成；
- 擦除后部分 LTFS label/MAM 仍可读，但 index 已不可用；
- 后续最小分区擦除后两个分区都不再存在 ANSI LTFS signature。

这验证了 short erase 是逻辑截断而不是安全清除，UI 不应把它描述为数据不可
恢复。

### minimum partition long erase

第一次测试暴露 Quantum 的进度 sense 为：

```text
sense key = 02h (NOT READY), ASC/ASCQ = 00h/18h
```

原判断只接受 sense key 0，因而误报错误；恢复保护仍成功执行，介质回到约
1.39 TiB 的未分区状态。修正为同时接受 key 0 和 NOT READY 后再次测试：

- long erase 进度从 0.0% 依次推进至 98.2%；
- long erase 本体约 5 分钟；
- 包含两次 rethread、分区创建和恢复的总耗时为 865 秒；
- 命令退出码 0；
- 最终介质为 partition 0、剩余/最大容量约 1.39 TiB；
- tapecpy 不再识别出 LTFS label。

## 5. 明确未测试：全带 long erase

本次按计划没有在真实介质上启动 `erase long`。LTO-5 全带 long erase 可能持续
数小时，当前只有以下代码级保证：

- 先 FORMAT type 0 移除 LTFS/其他分区，避免只擦除当前 P0；
- 使用 IMMED 启动，避免单条 SG_IO 1800 秒 timeout；
- REQUEST SENSE 每 30 秒报告进度；
- Quantum 的 NOT READY/00/18 已由最小分区测试覆盖。

未来有足够连续维护窗口时，必须补做一次完整真实测试，至少记录总耗时、进度
单调性、完成后的容量/分区状态、TapeAlert 和错误计数。测试期间不能重启服务、
杀死进程或对同一设备发送并发命令。

## 6. 当前限制

- 只验证了 Quantum LTO-5；其他厂商和代际可能返回不同的进度 sense；
- WORM、写保护和硬件加密介质仅依赖设备错误拒绝，尚无擦除前专门提示；
- 30 秒固定轮询周期尚未做按预计时长自适应；
- CLI 目前只显示设备路径和模式，未来 TUI 确认页还应显示 Barcode、Volume
  Name、驱动器序列号和当前分区。
