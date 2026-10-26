# 校验管线提交前复查（2026-10-26）

## 范围与结论

复查共享SHA1/SHA256 worker、ExtentReadCursor接入、SHA1只读批次审计和实验runner；设备命令编码及索引/VCI写入算法不变。修复以下问题后，暂未发现阻塞本次提交的问题。没有将审查理解为新平台、恢复架构或通用备份产品开发。

## 已修复

1. SHA1审计原先根据错误文本包含“SHA-1 不匹配”来决定是否继续，可能把类似文本的设备错误错误分类。新增VerifyError::DigestMismatch，携带算法、expected和actual，其他I/O/取消错误明确停止。回归注入含同样文本的设备错误，确认不继续；真正摘要不匹配则记录actual后继续后续文件。
2. 部分record切片回收时Vec容量可小于1MiB，后续resize可能倍增为超过1MiB。读取前reserve_exact恢复record容量，避免几何增长。40次部分record循环复用回归覆盖超过32个令牌的一轮循环。
3. runner原先用外部inventory文件数推断完整终态；现以实际批次事件的files_total为准。日志/进度/辅助输出错误不再panic或静默报成功，而是请求取消、回收worker并返回error。新增终态分类与进度I/O错误回归。
4. SHA1预检增加重复文件路径拒绝；恢复依据必须匹配当前路径/长度/SHA1，跳过记录明确标记skipped_previous_pass。未恢复用户已要求去掉的UUID/generation硬门槛。
5. worker失败后的再次调用返回错误，避免对取走的join handle或关闭的input unwrap；新worker创建失败保留写入已提交的错误上下文。

## 复核的不变量

- 唯一设备owner和lease负责定位/READ，worker仅消费字节。
- 32个buffer令牌约束在途record数，跨线程move并回收；额外最多一条共享record缓存。预算不包括线程栈、消息及分配器内部暂态开销。
- 每文件Finish屏障保证所有payload已hash，再比较长度和摘要；空文件、跨文件reset、非零偏移和共享record均覆盖。
- 队列等待每100ms检查取消；SCSI命令不被并发中断，取消等待现有调用返回。
- 关闭输入并join worker；I/O错误、摘要错误和取消不能发布验证通过。
- SHA1审计不写磁带；完整摘要错误可以继续下一文件，设备错误立即停止。

## 检查

完整cargo test --all-targets：207项库测试和2项runner测试通过，11项硬件/性能测试未自动运行。Clippy --all-targets --all-features -D warnings、rustfmt、git diff --check通过；release all-targets构建单独验证。IPC测试使用允许本机socket的环境运行。

新增覆盖typed mismatch/actual摘要、类似错误文本的I/O失败、后续文件继续、关闭worker再次调用、文件开始时取消不发READ、部分record预算、恢复依据/重复路径、保存失败与终态分类。

此前真机证据：共享SHA1管线完成A/B各约1.375TB全盘校验；SHA256同一93.36GB文件两轮管线约141MB/s，与仅读取一致，均通过。详见SHA256_LONG_STREAM_RESULTS_2026-10-26.md、SHA1_AUDIT_PIPELINE_2026-10-25.md。上述真机二进制早于本次防御性修复，不能将其SHA当作当前构建，也没有重复启动数小时硬件测试。完整record稳态的数据路径与线程拓扑不变；部分record分配修正有针对性软件回归。

## 保留限制

SHA1审计目前为核心API与实验example，不是新增正式CLI/TUI功能；明确拒绝无唯一有效SHA1、symlink、稀疏/乱序extent。示例恢复输入目前针对旧Python的成功记录格式，不是任意JSON结果迁移器。进度快照使用原子rename，终态事件sync_all，但不宣称断电事务或自动重启续跑。诊断代码/计时只存在于显式测试构建。

## 提交范围

代码、依赖、相关实验入口、文档和校验性能证据。与本特性无关的G3260内存排查报告、网络部署脚本和聊天交接资料保留为未跟踪文件，不删除、不批量加入提交。未修改或操作远端磁带及已完成的检查证据。
