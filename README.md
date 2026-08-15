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

## 编译与部署

### 1. 准备系统

当前只支持 Linux。编译需要 Git、C 编译工具和支持 Rust 2024 edition 的稳定版 Rust
（`rustc 1.85` 或更新版本）；TAR 工作流需要 GNU tar。推荐通过
[rustup](https://rustup.rs/) 安装 Rust。

Fedora：

```bash
sudo dnf install git gcc tar
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Debian/Ubuntu：

```bash
sudo apt install git build-essential tar curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

重新打开 shell，或按 rustup 的提示加载 Cargo 环境。然后确认 `rustc --version` 和
`cargo --version` 可以正常运行。

### 2. 编译

```bash
git clone https://github.com/ainretriever/LTFSCopyTUI.git
cd LTFSCopyTUI
cargo build --locked --release
```

可先在源码目录中运行：

```bash
./target/release/tapecpy --help
./target/release/tapecpy list
./target/release/tapecpy
```

不带参数启动主 TUI。`list` 应当列出系统识别到的磁带机。

### 3. 安装可执行文件

tapecpy 是单个可执行文件，不需要安装 OpenLTFS、FUSE 或常驻 systemd 服务。安装到当前
用户目录：

```bash
install -Dm755 target/release/tapecpy "$HOME/.local/bin/tapecpy"
```

确保 `$HOME/.local/bin` 位于 `PATH`，之后可以直接运行 `tapecpy`。也可以由管理员安装到
所有用户可用的位置：

```bash
sudo install -Dm755 target/release/tapecpy /usr/local/bin/tapecpy
```

长时间读写任务会在用户确认开始操作时派生可脱离的 runner；关闭 TUI 或断开 SSH 不会
终止它。任务状态、日志和本机 IPC 默认保存在
`$XDG_STATE_HOME/tapecpy/jobs`，未设置该变量时保存在
`$HOME/.local/state/tapecpy/jobs`。安装时不需要另建服务账户或后台服务。

### 4. 配置磁带设备权限

Linux 通常通过 `st` 和 `sg` 内核模块提供 `/dev/nstX` 与 `/dev/sgX`。如果设备节点没有
出现，可先检查驱动和设备：

```bash
sudo modprobe st
sudo modprobe sg
lsscsi -g
ls -l /dev/nst* /dev/sg*
```

`lsscsi` 不是运行依赖，只用于部署诊断，可从发行版的 `lsscsi` 软件包安装。tapecpy 的
运行用户必须同时具有对应 `/dev/nstX` 和 `/dev/sgX` 的读写权限。请按照发行版的设备
所有者组或 udev 规则授予权限，并在修改用户组后重新登录；不要长期依赖把设备节点设为
`777`。设备节点编号可能在重启或重新连接后变化，tapecpy 会通过 sysfs 匹配同一台驱动器
的两个节点。

如需从 NFS 或 CIFS 读写，挂载点必须在启动 tapecpy 和 detached runner 的同一用户环境
中持续可见，并且该用户必须具有源目录和目标目录的相应权限。任务运行期间不要卸载或
替换挂载点。

部署完成后建议先插入允许覆盖的测试带，运行 `tapecpy list`，再从 TUI 检查驱动器身份、
MAM 和写保护状态。Format、Erase、RAW Write 或 TAR Write 的首次验证不要使用唯一的数据
副本。

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
