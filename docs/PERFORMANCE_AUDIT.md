# 性能审计记录

## 2026-08-21 Read 路径审计

### 已修复：旧单文件 Read 路径绕过有界管线

此前 CLI `read -o` 和 detached `job start-read` 的兼容路径直接在磁带读取线程中
执行 host `write_all`，并且每个 extent 都无条件 `LOCATE`。慢速 NFS/CIFS 目标会直接
阻塞磁带读取；相邻 extent 还会被重复定位，破坏 streaming。

现在两条路径都生成单文件 `ReadPlan`，复用 `execute_read_plan` 的 512 MiB bounded
destination pipeline、物理 extent 顺序和有界文件句柄策略。stdout 仍保持直接流式输出，
但会复用 record buffer，并且只有当前位置与下一个 extent 不连续时才发送 LOCATE。

### 尚未处理：遥测查询插入数据流

Read/Write 数据通路约每 5 秒通过同一 `TapeSession` 查询 diagnostic page。该查询会在
当前 READ/WRITE 命令之间插入多个 SG_IO 命令，可能影响磁带 streaming。需要在真实设备上
比较 telemetry 开启/关闭时的吞吐、位置连续性和速度波动。

### 尚未处理：JobState 持久化热路径

Detached runner 的性能事件会触发完整 JobState 序列化、临时文件写入、`sync_all` 和
rename。当前有 250 ms/事件节流，但每秒性能样本仍可能同步状态文件。需要比较本地 SSD、
NFS/CIFS 状态目录和高负载主机上的 tape starvation，再决定是否引入独立 persistence
worker。

### 尚未处理：record buffer 清零和重复分配

磁带 READ buffer 当前按 1 MiB 上限 resize，source writer 也按 block size 初始化 buffer；
TAR producer 每个 record 新建 zeroed Vec。需要用 `perf stat` 和不同 record size 测量内存
带宽、分配器开销和实际 tape throughput，再决定是否引入未初始化 buffer 或 buffer pool。

### 尚未处理：source preflight 重复 metadata I/O

Write source 可能经历 TUI scan、runner re-plan 和 source pipeline 再次 open。大量 NFS/CIFS
小文件时会增加首条 WRITE 前等待和 metadata 往返，但不一定影响稳定数据阶段。需要单独测量
scan、runner startup、first WRITE 和 steady-state 四个时间点。

### 建议基准

- 相同测试带分别运行 TUI Read、detached Read 和 CLI `read -o`；
- 对本地、NFSv4、CIFS destination 分别记录 tape throughput、buffer pressure、总时间和
  SHA-256；
- 用 `perf stat -d` 观察 CPU/cache/memory bandwidth；
- 用 `strace -f -ttT` 统计 `SG_IO`、`LOCATE`、`READ POSITION`、`fsync` 和 host write；
- 在真实测试带上比较 telemetry 开启与关闭；
- 任何改变 record size 或 Write 管线的测试只能使用专用可破坏测试带。
