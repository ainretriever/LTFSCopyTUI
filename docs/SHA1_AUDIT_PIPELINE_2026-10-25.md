# 旧带SHA1检查复用读取/哈希管线（2026-10-25）

用户要求中断Python SHA1旧带检查并改用共享Rust管线。旧进程已停止，保留tapeserver /home/ain/tapecpy-audit-20261025/tape-A/full-audit全部证据。archive_0040.tar为完整通过，34376120320字节，SHA1 FF924A392C3B3A11DCDDECE24BC3DAB836CFE966；其后archive_0041.tar刚开始时收到SIGTERM，read_exit_code=-15、0字节，该记录是人为中断，不计为数据损坏或完整校验失败。

## 变化与范围

RustCrypto sha1 0.10.7加入Cargo.lock；VerifyPipeline增加SHA1选择，复用既有唯一设备owner、32个buffer令牌（最多约32MiB+cursor一条record缓存）、顺序extent读取和SHA worker。写入仍生成SHA256，不为显示兼容而增加写入SHA1，不修改任何带上索引/VCI。

Application新增audit_sha1_volume，批次持有一个lease/session，按文件首extent物理位置排序；每文件长度和摘要检查后才完成。worker不访问设备，无payload stdout管道或Python复制，连续文件保留cursor。完整摘要不匹配会报告并继续其余文件；设备错误、取消或不支持的索引语义禁止声称全盘通过。

按用户明确要求不固定核查UUID/generation。恢复进度仅按路径/长度/当前索引SHA1匹配已通过记录，跳过archive_0040.tar；archive_0041.tar从头重读。库存清单记录E6002AL5（UUID24c8b67d-5a7f-4fb4-81dd-923e1487abd3、generation21），仅用于报告，不作为运行时门槛。

审计前验证所有普通文件都有唯一40位十六进制SHA1，接受大小写并规范为小写；拒绝缺失/重复/畸形摘要、符号链接以及未支持的稀疏/乱序extent。避免把不支持的文件误算成通过。

## 软件验证与部署

cargo test --all-targets：202 passed、11 ignored；Clippy all-targets/all-features -D warnings及格式检查通过。SHA1增加abc、空字符串、百万a标准向量，多buffer顺序及同worker跨文件重置、摘要失败；索引预检新增大小写规范、非零byteoffset保留及不支持格式拒绝测试。原SHA256、跨record/跨分区extent、worker故障/取消测试仍通过。

正式tapecpy SHA256：ee780a8576c2848e8162c0d724ab85c3acba0e1b359bae2706a754cf172e64ef。
审计runner SHA256：6f041883ffc19e90cbbd52a45bab1a38fa102b2374c3493086371fae26cff5d5。
原程序备份/home/ain/tapecpy-audit-20261025/tapecpy.before-sha1；现有~/.local/bin/tapecpy链接不变。

后台启动审计runner /home/ain/tapecpy-audit-20261025/audit-sha1，PID8956。输出目录/home/ain/tapecpy-audit-20261025/tape-A/full-audit-rust-sha1；progress.json实时进度、events.jsonl逐文件终态，日志tape-A/audit-rust-sha1.log。本轮不守候数小时全带完成；未完成前不得宣称整盘数据正确。请保持磁带A装载，不并行运行其他磁带命令。

## 启动后短采样

已核实跳过archive_0040.tar，archive_0041.tar正在读取，status=running，resumed_previous=1；没有新增完整文件终态。一次5秒增量采样约161.87MB/s（应用payload），高于旧Python检查时一次110.87MB/s短采样；两次不是严格同位置AB测试，旧带压缩开启，短时值也受驱动器缓冲/压缩影响，不当作长期物理带速或全盘加速保证。用户听到走带明显加快，与当前短采样方向一致。

## 2026-10-26 提交前修订

摘要失败使用明确类型而不是错误文本判定，终态新增actual_sha1和files_total；runner统计使用当前批次总数，并把输出/进度保存失败作为错误返回，禁止误报完成。重复路径和错误恢复依据在读取前拒绝，部分record循环复用改为精确容量预留。既有远端A/B校验由此前二进制完成；本修订的软件回归通过，未将历史真机二进制SHA冒认为当前源码构建。
