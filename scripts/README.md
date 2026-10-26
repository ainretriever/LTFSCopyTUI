# 性能验证工具

这些工具来自2026-10-22至24的LTO5实机研究，运行方式和原始结果见
[性能汇总](../docs/PERFORMANCE_SUMMARY_2026-10-24.md)。不是安装程序，也不由正常TUI或普通cargo test自动执行。

## 普通软件检查

```sh
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

默认测试不需要磁带，硬件/性能测试均为ignored。不要用一次全局`--include-ignored`或`--ignored`来运行所有测试：其中部分会向磁带追加数据，必须按指定test名称和条件单独执行。

## 可独立使用的工具

|工具|用途/输入|
|---|---|
|`prepare_perf_data.py`|创建全新的不可压缩数据集和SHA清单；使用`--help`查看大小和文件数。|
|`verify_perf_data.py`|按清单独立验证恢复内容；使用`--help`查看选择路径和输出路径参数。|
|`watch_perf_job.py`、`summarize_perf_job.py`|持续保存任务快照/进程观测和分析阶段吞吐，使用`--help`查看参数。|
|`analyze_ioctl_trace.py`、`analyze_channel_samples.py`|离线分析已有SG_IO跟踪和通道计数；不会自行操作磁带。|
|`run_perf_suite.py`、`run_perf_followup.py`|串行真实写入/恢复验证，依赖指定实验目录、helper和既有初始job；包含磁带追加写入，不是通用基准入口。|

`examples/`中的Rust辅助程序分别用于独占lease后的温度查询、分区容量查询、读取切片以及启动目录恢复。部分使用实验设备`/dev/sg0`；执行前阅读源文件和参数。它们只在显式`cargo run --release --example ...`时运行。

## Host-output单项示例（不访问磁带）

目标目录必须不存在，父目录必须存在。以下例子创建16MiB内容并逐文件验证，不自动删除结果：

```sh
TAPECPY_RESTORE_SPEC='{"root":"/tmp/tapecpy-restore-host-new","size":1048576,"count":16}' \
  cargo test --release --lib app::restore_tests::restore_host -- --ignored --exact --nocapture
```

`record`默认512KiB，可显式提供。输出`RESTORE_RESULT` JSON中总耗时不含最后SHA复核。将root放到实际已挂载的NFS/SMB目录下才能测试该后端。

`app::restore_tests::restore_queue`是暂停真实worker、填满默认512MiB预算后取消的探针，环境参数为`{"size":4096,"record":4096}`。它会占用超过512MiB进程内存，不读取磁带，也不代表完成了文件内容验证。

## 实验室专用脚本

`network_test/`中的驱动和分析脚本保留原实验命名、绝对目录、NAS挂载点、设备路径及before/after二进制布局，目的是解释和复现实验记录，不能原样当作任意主机的部署配置。

- `compare-source-optimization.py`、`compare-stable-source.py`及对应finish脚本：源读取A/B与实验性跳过校验对照。跳过校验仅存在于测试构建。
- `research-restore*.py`、`restore-actual-queue-control.py`、`restore-granularity-calibration.py`：恢复输出/缓冲/实际磁带研究。
- `test-restore-sync.py`、`test-restore-buffer.py`：同一ignored测试入口的before/after对照，最后从既有夹具恢复到SMB；所需夹具必须事先存在。
- `restore-sync-control.py`、`restore-syscall-controls.py`、`restore-protocol-monitor.py`：文件同步、系统调用和协议计数控制。
- `run-tests.py`、`run-small-host.py`、`small-file-baseline.py`和分析/汇总脚本：先前大小文件与网络实验。

驱动可能执行大量主机I/O、磁带追加写入或删除自己本次生成的输出目录，部分脚本没有main guard。先阅读源文件并调整BASE/ROOTS、夹具、设备和二进制路径；不要import这些脚本来探测接口。编译测试二进制使用`cargo test --release --lib --no-run`，以Cargo输出的Executable路径为准，不假设固定文件名hash。

`setup-server.sh`、`setup-client.sh`及`remove-old-nfs.sh`只用于原实验环境的管理员配置记录。它们会修改服务/共享/挂载配置，旧NFS清理脚本还会停止并恢复NFS服务；已包含特定hostname/IP/接口保护。配置细节见[网络实验部署记录](../docs/NETWORK_TEST_SETUP_2026-10-23.md)。SMB凭据由现场随机生成、保存在600权限文件中，未纳入仓库。

## 构建和计时边界

分阶段计时、SG_IO跟踪、伪随机夹具和实验开关均通过`cfg(test)`隔离；正式release不包含这些测试入口。测试二进制和正式二进制是不同构建，报告SHA必须按用途区分。历史脚本与数据不能假定新版本仍有相同事件数量、同步策略或字段含义，比较前先阅读对应报告的限制说明。

## 写后校验只读 AB/BA（2026-10-25）

`app::verify_pipeline::hardware::verify_ab`是显式ignored测试。仅在独占lease后读取已有LTFS文件，顺序为serial/pipeline/pipeline/serial，不写磁带。使用当前release测试构建，`TAPECPY_VERIFY_AB`需提供`path`、`bytes`和该前缀的独立`sha256`；要求单extent，大小不得超过文件与extent长度。命令示例（自行替换已核对的夹具与摘要）：

```sh
TAPECPY_VERIFY_AB='{"path":"/verified-fixture.bin","bytes":4294967296,"sha256":"EXPECTED_PREFIX_SHA256"}' \
  cargo test --release --lib app::verify_pipeline::hardware::verify_ab -- --ignored --exact --nocapture
```

每轮输出`VERIFY_AB_RESULT`，stream_seconds从首次READ开始直至哈希完成，排除首次LOCATE；wall_seconds另含该轮LOCATE，不含测试开始时的卷识别。read_io_seconds为READ调用累计耗时，不是可独立当作持续带速的指标。串行路径仅保留在cfg(test)中；正式release没有切换或跳过校验的开关。设备、文件、二进制和CPU配置必须保持一致，不能运行全局--ignored。结果见docs/VERIFY_PIPELINE_RESULTS_2026-10-25.md。

## SHA1旧带只读审计

examples/perf_audit_inventory.rs导出解析后的索引及文件/哈希清单（不是原始索引字节保全）；examples/perf_audit_sha1.rs使用共享Rust管线执行整批审计。runner参数为inventory目录、旧Python审计files.jsonl、全新的输出目录。只从旧记录中恢复verified=true、read_exit_code=0、实际/预期SHA1和长度均一致的文件；同路径/长度/摘要还必须匹配当前索引。此runner目前是本轮实验入口，不是正式CLI功能或任意旧JSON格式转换工具。SIGTERM/SIGINT请求record边界取消，等待SCSI调用返回。结果events.jsonl记录逐文件终态，progress.json每秒保存进度，不能把reading或skipped_previous_pass当作本轮重新读取通过。详见SHA1_AUDIT_PIPELINE_2026-10-25.md。

## 长流校验诊断与提交前复查

同一verify_ab测试可在TAPECPY_VERIFY_AB中提供modes列表（read/serial/pipeline），显式设为read/serial/pipeline/pipeline/serial/read执行长流对照。read仅作为读取基线，不声明摘要验证通过。测试输出包含buffer等待、READ间隙及逐秒payload样本，指标均由cfg(test)隔离。scripts/g3260_test/run_long_verify.py为本轮实验的固定路径驱动，不是通用安装或自动执行入口；使用前阅读并替换夹具。长流结果见../docs/SHA256_LONG_STREAM_RESULTS_2026-10-26.md。

SHA1审计runner依赖当前可读索引，明确拒绝缺失/重复/格式错误的SHA1、符号链接和稀疏/乱序extent；普通写后校验仍使用SHA256。对日志/进度保存失败返回错误并请求取消。核心审计API与实验example已可用，本次不新增正式CLI/TUI菜单。
