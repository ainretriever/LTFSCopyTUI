# tapecpy 开发说明

tapecpy 是一个面向 Linux 的磁带操作工具，项目名称为 **LTFSCopyTUI**：在 Linux 上重新实现 LTFSCopyGUI 提供的核心能力，并以 TUI 作为主要交互界面。

在进行架构调整或较大功能开发之前，请先阅读：

* `docs/PROJECT.md`
* `docs/WRITE_WORKFLOW.md`
* `docs/ARCHITECTURE.md`

## 基本原则

* Linux 是当前唯一的主要目标平台。
* LTFS 核心功能不能依赖通过 FUSE 挂载的 LTFS 文件系统。
* 磁带机是有状态的顺序设备，设备状态必须由单一组件统一管理。
* 磁带/SCSI 设备访问、LTFS 格式逻辑和 TUI 表示层必须彼此分离。
* TUI 是主要交互界面，但核心功能不能依赖 TUI 才能运行。
* 优先选择行为明确、状态可观察的实现方式，避免把磁带机隐藏在普通文件系统抽象后面。
* 不要因为某个功能容易实现，就擅自扩大项目范围。
* 当前首要目标是完成正常的 LTFS 写入工作流。
* RAW、TAR 和高级恢复功能不得妨碍 LTFS 主线架构的完成。

## 开发流程

进行较大的功能开发前：

1. 阅读相关设计文档；
2. 检查现有实现；
3. 说明准备修改的部分在整体架构中的位置；
4. 指出仍未解决的设计问题；
5. 确认设计没有明显冲突之后再开始实现。

不要在没有理解设备状态、LTFS 数据结构和当前工作流的情况下直接进行大规模重构。
## LTFSCopyGUI 参考实现

tapecpy 的很多功能需求来源于 LTFSCopyGUI。

可选的本地参考源码 checkout 位于：

`references/LTFSCopyGUI/`

获取方式及许可证注意事项见 `references/README.md`。该源码不随 tapecpy 分发。

上游项目：

`zhaoyangwx/LTFSCopyGUI`

### 使用原则

LTFSCopyGUI 是 tapecpy 的重要功能和协议实现参考，但不是 tapecpy 的目标架构。

在处理以下问题时，应主动检查 LTFSCopyGUI 的实现：

* LTFS label 和 index 的解析、生成；
* LTFS partition 操作；
* LTFS 文件 extent；
* LTFS 格式化流程；
* SCSI 磁带机控制；
* MAM；
* LOG SENSE；
* TapeAlert；
* 磁带容量和位置查询；
* 错误率统计；
* 写入时 hash 和写后校验；
* erase、load、unload、rewind 等磁带操作；
* LTO 驱动器厂商差异和兼容处理。

### 不允许的做法

不要因为 LTFSCopyGUI 已经实现某项功能，就直接复制它的程序结构。

特别不要继承以下设计：

* Windows GUI 与核心逻辑耦合；
* Windows 专用设备访问方式；
* VB/.NET UI 导致的状态管理结构；
* 为 WinForms 服务的线程和事件组织方式；
* 与 tapecpy 新架构冲突的全局状态。

处理一个功能时，应区分：

1. LTFSCopyGUI 证明了什么；
2. 它发送了哪些设备命令；
3. 它如何解释 LTFS 数据；
4. 哪些行为来自 LTFS/SCSI/LTO 规范；
5. 哪些行为只是 LTFSCopyGUI 自己的实现选择。

tapecpy 应重新实现所需功能，而不是机械翻译 LTFSCopyGUI。

### 开发流程

如果任务涉及已有的 LTFSCopyGUI 功能：

1. 先阅读 tapecpy 设计文档；
2. 查找 LTFSCopyGUI 对应实现；
3. 总结它实际执行的设备/格式操作；
4. 必要时对照 LTFS、SCSI 或厂商文档验证；
5. 根据 tapecpy 架构设计 Linux 实现；
6. 再开始修改 tapecpy 代码。

除非用户明确要求，不要把 LTFSCopyGUI 源文件直接复制到 tapecpy。
