# LTFS 格式化工作流审阅记录（2026-08-09）

本文记录 tapecpy 直接格式化 LTFS 介质的实现、与 LTFSCopyGUI/OpenLTFS 的
对照结论，以及在真实磁带机上的验证结果。

## 1. 实现范围

当前 `tapecpy format` 实现不依赖 FUSE 或 `mkltfs`，由 `FormatSession` 独占
设备并顺序完成：

1. 校验 6 位大写字母数字 Barcode 和 Volume Name；
2. 读取并修改 Medium Partition mode page（0x11），用 MODE SELECT(10)
   建立 LTFS 的 index/data 两个分区；
3. 发送 FORMAT MEDIUM（format type 1）；
4. 设置可变块模式并写入 LTFS MAM application attributes；
5. 分别在 data partition 1 和 index partition 0 写入 ANSI VOL1、XML label
   和 generation 1 index；
6. flush 驱动器缓冲，读取 Volume Change Reference，再向两个分区写入
   Volume Coherency Information（VCI）；
7. 返回 volume UUID、generation 和两个 index 位置。

命令行入口是：

```text
tapecpy format <6位Barcode> <Volume Name> [选择器] --force
```

格式化会不可逆地覆盖卷上的现有 LTFS 内容，因此必须显式给出 `--force`；
缺少该参数时，程序会在发现或打开设备之前拒绝执行。

## 2. LTFSCopyGUI 参考结论

优先对照了 `references/LTFSCopyGUI/LTFSCopyGUI/TapeUtils.vb` 中的
`mkltfs`、`SetMAMAttribute`、`WriteVCI` 和 `Flush`：

- 分区由 Medium Partition mode page、MODE SELECT 和 FORMAT MEDIUM 建立；
- LTFS MAM attributes 用 WRITE ATTRIBUTE（0x8D）写入，并在 CDB 中指定分区；
- 初始卷在两个分区都使用 `[VOL1][FM][XML label][FM][FM][index][FM]`；
- VCI 写入前先用零计数 WRITE FILEMARK flush，再读取 VCR；
- VCI 同时写入 index 和 data 分区。

tapecpy 复用了这些设备/格式行为，没有复制其 WinForms、全局状态或线程结构。
设备状态仍由单个 `TapeSession` 管理，LTFS 文档生成与 SCSI 传输保持分层。

另外对照 OpenLTFS 2.4.8.4 的分区 mode page、FORMAT MEDIUM 和 VCI
descriptor 实现，采用一个额外分区、IDP、单位 0x09、P0 最小值 1、P1 使用
剩余容量的软分区配置。当前没有实现 LTFSCopyGUI 中可选的 SET CAPACITY 和
format initialization type 0 路径。

## 3. 初始卷结构

当前生成 LTFS 2.4.0，默认块大小 512 KiB、启用压缩。初始 index 关系为：

```text
index partition: generation 1 @ a:5
  previousgenerationlocation -> b:5

data partition:  generation 1 @ b:5
  无 previousgenerationlocation
```

两个 XML label 使用同一个 UUID；index 的 volume UUID、分区名称和标签一致。
ANSI VOL1 label 写入 6 位 Barcode 和 LTFS 标识。XML serializer 会转义
Volume Name 中的 XML 特殊字符，parser 也保留 entity 前后的空白。

## 4. MAM 与 VCI

格式化写入以下 application attributes：

- 0x0800 Application Vendor：`OPEN`；
- 0x0801 Application Name：`tapecpy`；
- 0x0802 Application Version；
- 0x0803 User Medium Text Label（Volume Name）；
- 0x0804 Date and Time Last Written；
- 0x0805 Text Localization Identifier：UTF-8（0x81）；
- 0x0806 Barcode；
- 0x080B Application Format Version：`2.4.0`；
- 0x080C Volume Coherency Information。

真实设备测试发现，只在写完 index 后直接读取 VCR 会使首次 OpenLTFS 挂载
执行 full medium consistency check。LTFSCopyGUI 的 `WriteVCI` 在读 VCR 前
调用 `Flush`；补上相同的 WRITE FILEMARK count=0 后，首次挂载不再触发检查。
这说明 VCI 必须引用完成落带后的 VCR，而不能使用缓冲尚未提交时的值。

## 5. 真实设备验证

设备为 Quantum ULTRIUM 5、固件 3210、`/dev/sg1`，介质 Barcode 为
`E6008A`。使用 tapecpy 格式化出的卷：

```text
Volume Name: tapecpy format flush
Volume UUID: 90492284-35d4-436a-bbee-12121ae0fbac
generation 1: a:5 -> b:5
```

验证结果：

1. tapecpy 能读取 label、MAM 和 generation 1 空 index；
2. OpenLTFS 2.4.8.4 首次只读挂载成功，没有执行 full medium consistency
   check；
3. 在该卷上用现有 WriteSession 写入 21 字节文件，generation 变为 2，
   tapecpy 读回 SHA-256 与源文件一致；
4. OpenLTFS 能挂载 generation 2，并读回相同 SHA-256。

generation 2 首次由 OpenLTFS 挂载时仍会执行 full medium consistency check，
因为现有 WriteSession 尚未在普通写入完成后更新 MAM VCI。OpenLTFS 更新 VCI
后再次挂载不再检查。这是写入里程碑已知遗留项，不是格式化初始布局错误。

## 6. 当前限制与下一步

- 只在 Quantum LTO-5 上验证了软分区格式化；其他厂商、LTO 代际、WORM、
  写保护和不同 mode page 长度仍需兼容性测试；
- FORMAT MEDIUM 可能长时间占用设备，SG_IO 磁带命令当前统一使用 1800 秒
  超时；这会造成取消不及时或故障反馈延迟，后续仍需按 opcode/阶段细化；
- 尚未实现格式化过程取消后的状态判定与恢复提示；
- 普通 WriteSession 的 VCI 更新仍待实现；
- 当前路线图的下一个垂直切片是 erase，再进入完整 write workflow。
