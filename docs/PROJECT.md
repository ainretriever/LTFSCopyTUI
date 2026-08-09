# tapecpy 项目定义

## 1. 项目起源

tapecpy 最初的设想是 **LTFSCopyTUI**。

LTFSCopyGUI 提供了很多实用的 LTFS 直接读写、磁带机控制和诊断能力，但是它主要面向 Windows，其作者也明确没有把 Linux 作为主要支持平台。

tapecpy 的目标是在 Linux 上重新实现这种工作方式，并使用 TUI 作为主要交互界面。

项目目标并不是简单实现一个“磁带版 cp”。

## 2. 要解决的问题

目前 Linux 上常见的 LTFS 使用方式是：

```text
磁带机
  ↓
OpenLTFS / HPE LTFS
  ↓
FUSE
  ↓
挂载目录
  ↓
普通文件操作
```

这种方式把很多磁带机实际行为隐藏了起来。

用户很难直接观察：

* 当前磁带位置；
* 当前所在 partition；
* LTFS index 什么时候更新；
* 磁带什么时候发生定位和倒带；
* 实际写入速度；
* 写入速度变化；
* recovered write/read error；
* hard error；
* TapeAlert；
* 驱动器健康状态；
* SCSI sense 信息；
* 操作失败发生在哪一个阶段。

tapecpy 希望直接控制磁带设备，并在实现 LTFS 文件操作的同时，把这些状态明确展示给用户。

## 3. 核心理念

磁带不是普通的随机访问磁盘。

tapecpy 不应该试图把磁带完全伪装成普通目录，而应该把磁带的顺序访问特征和设备状态直接展示出来。

核心目标是：

> 在 Linux 上直接控制 LTO 磁带机，理解并读写 LTFS，同时让磁带的位置、状态、性能和错误情况对用户保持可见。

## 4. 主要用户界面

主要交互方式为 TUI。

TUI 应该适合长时间观察磁带写入过程，例如显示：

* 当前文件；
* 已写入容量；
* 当前速度；
* 平均速度；
* 速度历史；
* 当前磁带位置；
* recovered error；
* hard error；
* TapeAlert；
* 其他磁带机诊断数据。

同时提供 CLI，用于：

* 脚本；
* 自动化；
* stdin/stdout；
* 状态查询；
* 调试。

TUI 和 CLI 必须共享同一套核心实现。

## 5. 项目范围

tapecpy 的主要工作对象是：

```text
一台主机
+
一台磁带机
+
当前插入的一盘磁带
```

项目负责这盘磁带本身的读写、格式和设备操作。

以下功能目前不属于 tapecpy 的核心范围：

* 多盘磁带自动 spanning；
* 自动选择下一盘介质；
* 磁带库机器人管理；
* 全局磁带 catalog；
* 备份策略；
* 增量备份；
* 去重；
* 定时备份；
* 大规模介质调度。

这些功能未来可以由其他程序调用 tapecpy 来完成。

## 6. LTFS

LTFS 是 tapecpy 的主要工作模式。

tapecpy 应该直接实现 LTFS 的：

* label；
* partition；
* index；
* extent；
* 文件和目录；
* index 更新；
* 格式化；
* 读取；
* 写入。

核心 LTFS 操作不能依赖 FUSE mount。

tapecpy 必须遵守正式 LTFS 规范，而不是创建自己的 LTFS 变体。

## 7. RAW

RAW 模式提供直接的顺序磁带访问。

基本语义为：

```text
输入二进制流
    ↓
磁带 records
```

多个独立输入对象之间使用 filemark 分隔。

RAW 模式本身不保存：

* 文件名；
* 目录结构；
* 时间戳；
* 权限；
* 自定义 tapecpy metadata。

不要为了保存这些信息而给 RAW 添加私有 archive header。

如果需要保存文件系统 metadata，应使用 TAR 或 LTFS。

## 8. TAR

TAR 模式的基本结构为：

```text
文件 / 目录
    ↓
TAR encoder
    ↓
顺序字节流
    ↓
磁带
```

TAR 应尽量复用 RAW 的顺序数据通道。

tapecpy 不需要把 TAR 做成第二套文件系统。

## 9. LTFS 恢复

未来可以增加专门的 LTFS recovery 功能。

当正常 LTFS index 无法解析时，可以尝试：

* 扫描 index partition；
* 查找旧 generation；
* 扫描 data partition；
* 根据残存 extent 信息恢复文件；
* 对整个 partition 进行物理 RAW dump。

LTFS recovery 和普通 RAW 模式属于两个不同概念。

恢复功能不属于第一个 milestone。

## 10. 第一阶段目标

第一阶段只关注完整可靠的 LTFS 写入工作流：

```text
选择磁带机
→
检查介质
→
擦除 / 准备介质
→
LTFS 格式化
→
设置 barcode 和 volume name
→
选择数据
→
写入
→
监控设备状态
→
更新最终 index
→
可选完整校验
→
弹出磁带
```

RAW、TAR 和高级恢复能力不得阻塞这一目标。
