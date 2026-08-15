# tapecpy

tapecpy（项目名 **LTFSCopyTUI**）是一个面向 Linux 的 LTO 磁带工具。它直接控制
磁带设备并解析、读写 LTFS，不依赖通过 FUSE 挂载的 LTFS 文件系统；主要操作界面为
TUI，同时保留可供脚本和诊断使用的 CLI。

项目受到 [LTFSCopyGUI](https://github.com/zhaoyangwx/LTFSCopyGUI) 启发，但采用面向
Linux 的独立架构和设备访问实现。

## 当前功能

- 发现磁带机，读取介质状态、MAM、TapeAlert 和累计健康信息；
- 装载、穿带、退带、弹出以及多种擦除操作；
- 直接识别、浏览、格式化、读取和写入 LTFS；
- 写入时计算 SHA-256，并可选择写后完整回读校验；
- 显示实时吞吐、缓冲状态和 16 通道读写错误率；
- 通过可脱离任务执行长时间 LTFS、RAW 和 TAR 读写，退出 TUI 或断开 SSH 不会终止任务；
- 将文件或目录以 RAW records 或 GNU TAR 字节流写入磁带，并恢复完整 RAW/TAR 镜像；
- 从 NFS、CIFS 和本地文件系统选择源数据或恢复目标。

## 构建与运行

需要 Linux、Rust 工具链以及能够访问的 SCSI generic 磁带设备（通常为 `/dev/sgX`）。
TAR 工作流还需要 GNU tar。

```bash
cargo build --release
./target/release/tapecpy
```

运行账户必须具有访问对应磁带设备的权限。启动 TUI 后选择磁带机，并从 Overview 页面
进入介质、LTFS 或 RAW/TAR 操作。CLI 命令可通过以下方式查看：

```bash
./target/release/tapecpy --help
```

## 安全提示

磁带机是有状态的顺序设备。Format、Erase、RAW Write 和 TAR Write 都可能不可逆地
覆盖现有数据；short erase 也不等同于安全物理销毁。请在操作前核对设备、磁带、写保护
状态、MAM 信息和确认页面。

本项目仍在开发和硬件兼容性验证阶段，目前的主要真机验证环境是 LTO-5。使用其他代际、
厂商或固件前，建议先用专用测试带验证完整工作流。

## 文档

- [项目目标与范围](docs/PROJECT.md)
- [架构设计](docs/ARCHITECTURE.md)
- [LTFS 写入工作流](docs/WRITE_WORKFLOW.md)
- [RAW/TAR 工作流](docs/RAW_TAR_WORKFLOW.md)
- [TUI 规格](docs/TUI_SPEC.md)
- [SNIA LTFS Format Specification 2.4（官方 PDF）](https://www.snia.org/sites/default/files/technical-work/ltfs/release/SNIA-LTFS-Format-2.4.0-TechPosition.pdf)

## 许可证

本项目采用 [Apache License 2.0](LICENSE) 发布。
