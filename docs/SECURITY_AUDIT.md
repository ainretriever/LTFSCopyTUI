# 安全审计修复记录

## Read destination symlink 逃逸

修复日期：2026-08-20

### 问题

LTFS Read 将磁带内容恢复到主机文件系统时，原实现使用普通路径 API：

```text
validate destination
→ create_dir_all(destination/path)
→ File::create(destination/path)
```

这些 API 会跟随目录符号链接。已有的目录 symlink 可能被 `is_dir()` 视为有效目录；
校验完成后，目标路径的目录也可能被同一用户进程、清理任务或 NFS/CIFS 服务端替换。
结果可能是恢复数据被写入 destination 之外。

该问题不需要攻击者接触磁带，也不依赖磁带机的机械状态。单用户且完全信任主机文件系统时，
实际攻击概率较低，但代码仍必须满足“不越过 Read destination symlink”的安全不变量。

### 修复方案

Linux Read 输出现在使用 `openat2`：

1. 用 `openat2` 打开 destination 目录本身，并使用 `RESOLVE_NO_SYMLINKS`；
2. 所有恢复相对路径都相对于该目录 fd 解析；
3. 使用 `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`，禁止 `..` 和任意 symlink 穿越；
4. 目录创建通过目录 fd 逐级执行 `mkdirat`，创建后再次用 `openat2` 校验；
5. 文件首次创建使用 `O_CREAT | O_EXCL`，重新打开 extent 文件也继续使用 `openat2`；
6. `syncfs` 使用同一个安全打开的 destination 目录 fd。

CLI `read -o` 和没有冻结 Read plan 的 detached Read 兼容路径也复用同一个安全文件创建
helper，不能绕过上述约束。

因此，即使 preflight 之后目录被替换为 symlink，实际文件创建也会失败，不会跟随到外部路径。

### 性能取舍

磁带读取和网络文件系统 I/O 仍然是主要耗时。`openat2` 通常每个文件只增加一次内核路径解析，
不会影响磁带数据吞吐。相比逐级 `openat(O_NOFOLLOW)`，它在深层目录和大量小文件场景下系统调用
更少；NFS/CIFS 仍需通过真实环境基准测试确认元数据操作开销。

### 测试

回归测试覆盖：

- preflight 前已存在的目录 symlink；
- preflight 通过后插入目录 symlink；
- 普通 Read 输出文件的创建、重新打开和 `syncfs` 路径。

该修复依赖 Linux `openat2` 系统调用，项目当前目标平台为 Linux。

## 截断 XML 被接受为 LTFS metadata

修复日期：2026-08-20

### 问题

原 `Label::parse_xml` 和 `Index::parse_xml` 在 quick-xml 返回 `Event::Eof` 时直接结束。
如果攻击或损坏输入已经包含所有必需字段、但缺少最终根元素结束标签，字段解析仍可能成功。
对 index 来说，这可能把不完整目录树或 extent 集合当成当前卷事实，并在后续重写 index 时
固化数据丢失。

### 文档合法性检查

所有 LTFS label/index 在字段解析前先经过共享的文档级校验：

1. 恰好存在一个根元素，且必须分别为 `ltfslabel` 或 `ltfsindex`；
2. 所有元素必须正确嵌套并闭合，EOF 时根元素必须已经闭合；
3. 根元素之外只允许 XML 空白、注释和 processing instruction；
4. 拒绝多个根元素、错误根元素、非法或重复 attribute；
5. LTFS metadata 不需要 DTD，因此明确拒绝 `DOCTYPE`。

该层只判断 XML 文档是否 well-formed 以及根元素身份。现有 label/index parser 继续负责
LTFS 必需字段、数字、布尔值、分区和目录/extent 语义；两层都通过后才产生内部模型。

### Index partition 损坏回退

只读卷识别采用以下有界顺序：

```text
index partition VCI target
→ data partition VCI target（仅在前者无效时）
→ index partition sequential scan（两份 VCI 都不可信时）
```

data partition fallback 只有在 label UUID、VCI UUID/generation/block、index UUID/generation、
实际物理位置和 index self-location 全部一致时才接受。成功后保留 index partition 的解析
错误，卷状态为 `IndexCopyMissing`、`safe_for_normal_write=false`；它只允许浏览和恢复。

顺序扫描会额外记录最后一个有效 index 后是否存在损坏或没有结束 filemark 的记录组。
出现这种情况时不采用旧 generation。Write runner 启动前仍重新扫描 index partition；
XML 尾部损坏或扫描失败会清除只读 fallback 结果并拒绝普通写入。

### 测试

自动测试覆盖完整文档、截断根元素、错配结束标签、多个/错误根元素、DOCTYPE，以及 data
partition fallback 的 UUID/generation/block/self-location 信任条件和降级一致性状态。

## Detached job IPC 资源耗尽

修复日期：2026-08-20

### 问题

原 IPC server 每接受一个 Unix socket connection 就创建一个新线程，并在没有读取总时限
的情况下等待换行结束的 JSON request。同一用户下的异常客户端可以建立大量连接后不发送
完整请求，使 runner 的线程和文件描述符持续增长。长时间 `Watch` 也没有独立配额，可能
占满全部请求处理能力并延迟 `Cancel`。

### 修复方案

IPC 改为固定资源模型：

```text
nonblocking accept
→ bounded queue (16)
→ fixed worker pool (8)
→ concurrent Watch slots (4)
```

队列已满时立即返回 busy，不创建线程。Watch 达到 4 个时只拒绝新的 Watch，剩余 worker
继续处理 Status 和 Cancel。request 上限为 16 KiB，持久化状态 response 上限保持 1 MiB。

请求读取和响应写入都使用 5 秒总截止时间。每次 I/O 前按剩余时间更新 socket timeout，
因此客户端即使持续发送或接收零碎字节，也不能让总占用时间无限延长。客户端等待 Watch
response 时使用用户请求的 Watch 时间（最多 30 秒）加 5 秒传输余量。server 关闭时先停止
accept 并关闭发送端，再等待固定 worker 退出，不遗留按 connection 创建的线程。

### 运行边界

项目按单用户模型运行，job socket 保持 `0600`。没有增加 `SO_PEERCRED` 或额外认证：同一
Unix 用户被视为可信，跨用户访问由文件权限和部署环境负责。该修复提供资源有界性和任务
可靠性，不试图抵抗已经取得同 UID 进程控制权的攻击者。

### 测试

自动测试覆盖总读取截止时间（包括持续零碎输入）、超大 request、Watch 配额释放、Watch
耗尽时 Cancel 仍可执行、固定队列满载，以及 connection sender 关闭后全部 worker 退出。

### 真实设备验收（2026-08-21）

测试机：`ain@tapeserver`，设备为 QUANTUM ULTRIUM 5，固件 3210，序列号
`HU1340YHGE`，`/dev/sg1`。本次设备不是 HP/HPE，因此结果只证明 Quantum LTO-5
上的工作流，不外推为 HP 兼容性证明。测试源为 `/mnt/nfs-test/nfs-test`，NFS4。

- 新版 release binary 部署为用户目录下的隔离名称 `tapecpy-audit-20260820`；
- 5,222,639,105 bytes NFS source detached Write 完成 generation 2，TUI/SSH attach 退出不影响
  runner，read-back verify 通过；
- 目标父目录 symlink 的 detached Read 在首条磁带读取前以 `ELOOP` 失败，输出目录
  保持为空；
- 受控截断 P0 当前 index XML 后，`volume` 从 P1 VCI block 定点恢复 generation 2，
  明确显示 `IndexCopyMissing`/只读警告；`diagnose --full` 保留 P0 XML 截断证据；
- fallback 状态下普通 Write 在首条 WRITE 前拒绝，最终报告
  `safe_to_retry=false requires_diagnosis=true`；
- IPC 注入 32 个连接时返回 `job IPC busy: connection queue is full`，runner 线程数
  保持 `NLWP=10`（主线程 + 8 worker 等固定线程及运行线程）；
- 5 个并发 Watch 中 4 个返回状态，第 5 个返回
  `job IPC busy: concurrent Watch limit reached`；独立 `job status/cancel` 仍可用；
- 1,371,290,978 字节 Read 的 SHA-256 与 NFS 源一致。

验收后重新 format 为 `TEST03 / Audit Restored`，复核结果为 generation 1、P0/P1
index 与两份 VCI 一致、`Healthy`、`safe_for_normal_write=true`；TapeAlert 无活动
flag，LOG SENSE corrected/uncorrected read/write 均为 0。验收临时文件、socket 压测
输出和临时 corruption helper 已清理。

## LTFS Read 重叠 extent

修复日期：2026-08-21

Read plan 现在按每个文件的逻辑区间 `[file_offset, file_offset + byte_count)`
排序并拒绝重叠 extent。这个检查同时存在于：

- 从 LTFS index 生成 ReadPlan 的 Application 路径；
- detached job 读取持久化 ReadPreflight 的校验路径；
- 执行 ReadPlan 前的最终校验路径。

稀疏文件中的 gap 仍然允许，零长度 extent 不占用区间。拒绝重叠可以避免损坏或被篡改
的 index 在物理顺序读取后互相覆盖输出文件，并错误报告 Read 成功。
