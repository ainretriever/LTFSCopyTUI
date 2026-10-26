# 写后回读校验管线：实现与实测（2026-10-25）

已将普通LTFS写后回读的设备读取与SHA256计算分离。G3260上同文件4GiB前缀只读AB/BA对照，串行均值100.39MB/s、新管线124.24MB/s，约提升23.8%；四次摘要全部匹配。正式release另完成16GiB正常写入、generation5索引/VCI提交和完整回读校验。尚未证明能稳定达到140MB/s。

## 架构与实现

阅读PROJECT、WRITE_WORKFLOW与ARCHITECTURE后，在Application层新增src/app/verify_pipeline.rs。唯一设备owner仍通过既有ExtentReadCursor定位、READ及维护连续位置/共享record缓存；SHA worker只持有有序字节，不能访问设备。批次内只有一个worker，文件末尾屏障检查长度和摘要后重置hasher，不逐文件创建线程。

固定32个可流转buffer令牌限制数据队列与allocation数量；每条record当前上限1MiB，总payload allocation最多约32MiB，加cursor最多一条record缓存。缓冲懒分配、move传递并回收，不增加整文件副本。mpsc底层为非阻塞发送队列，但producer必须先取得有限buffer令牌，文件Finish又逐项等待，因此数据和控制消息都不能无限积累。

producer等待buffer/结果时每100ms检查取消，提交前及worker每次处理前也检查token；正在执行的SCSI READ仍等待调用返回。读取失败保留原错误；长度、摘要、worker断开/panic均不能报告成功。所有退出路径关闭输入并join worker。SHA算法、索引扩展属性、双index/VCI事务及单一设备lease不变。RAW/TAR与普通恢复路径没有改动。

参考核对LTFSCopyGUI上游08fa2ec8的异步hash Task及Schema行为，只借鉴数据/哈希可重叠的行为，不复制Windows任务结构。没有新增SCSI命令或改变LTFS格式。

## 软件检查

- 普通cargo test --all-targets：199 passed、11 ignored。未全局执行ignored硬件测试。
- 新增5项worker专项：多buffer顺序和跨文件/空文件屏障，长度/摘要失败，32个令牌耗尽时阻塞并响应取消，READ失败保留原错误并回收worker，panic错误报告。
- 既有packed extent测试接入新管线，验证非零byteoffset、同record共享、跨块和跨分区，仍保持2次LOCATE/3次READ。
- Clippy all-targets/all-features -D warnings、rustfmt及git diff --check通过。

初次沙箱回归的IPC测试因socket权限失败，获准在允许本机socket的环境完整重跑通过。新增硬件测试为app::verify_pipeline::hardware::verify_ab，按精确名称显式运行。

## 同文件只读AB/BA

正常内存G3260主机.209，3.2GiB系统内存；Quantum LTO5 HU1340YHGE、测试带A00284L5。读取此前已独立验证成功的/g3260-nfs-20261025T122044Z.bin前4GiB，服务端独立SHA：bcf8e491369f34a2aafe776bef7ab6c4d4a7c8c79536a6b39ace692fb7cb6785。每轮8192条512KiB READ，使用相同ExtentReadCursor，每轮新cursor使首次LOCATE可分开计时。测试始终持有独占lease，不写磁带。

|顺序|路径|从首次READ至hash完成|吞吐MB/s|READ累计秒|含首次定位墙钟秒|
|---|---|---:|---:|---:|---:|
|A1|原串行|42.810|100.326|17.073|72.212|
|B1|新管线|34.580|124.203|34.549|62.736|
|B2|新管线|34.559|124.279|34.525|63.679|
|A2|原串行|42.754|100.459|17.351|71.843|

四次摘要均匹配。吞吐约提高23.8%，数据段耗时约减少19.2%。串行基线只保留在cfg(test)，正式程序无切换/跳过校验选项。wall_seconds不含测试开始时的一次卷识别；READ累计耗时不应单独换算成持续带速。新管线READ调用内等待增多，总数据段却缩短；不能将这一现象直接归因于某个磁带内部机制，也没有测量可归因的物理倒带次数。

两次各4GiB且AB/BA顺序一致，说明本机当前条件下收益可复现；样本仍小、未锁定CPU governor/频率、未监测每条READ的物理走带状态，不推广为所有磁带机或长期稳定吞吐。该测试未包含温度/BER额外周期采样，沿用现有verify行为。

## 正式16GiB写后校验验收

部署正式release后，以既有NFS16GiB源追加/g3260-nfs-20261025T124914Z.bin。源SHA：50015dffbb5e17263f347980b5c00616b0087237e9f34c54991ede9399621004。任务完成、generation5、index_committed=true、verification=passed；corrected/hard read/write错误增量均0，无活动TapeAlert，磁带保持装载。旧文件全部保留。

总墙钟553.28秒，Preparing84.32秒、WritingData168.44秒、FinalizingDataIndex9.04秒、SyncingIndexPartition32.15秒、UpdatingCoherency4.02秒、Verifying235.71秒（各阶段约1秒观测误差）。verify包含定位，不是纯读取带速。逐秒状态仍沿用原有verify指标，不能将残留写入速率当成回读速度。

本轮写入稳定窗口payload/墙钟106.60MB/s、逐秒带速中位数136.61MB/s，低于此前正常内存轮次136.76/144.41MB/s。写入算法未改；新worker在提交之后才创建，不能仅凭先后认定该写入下降由新校验实现造成，但也没有排除所有环境原因。写入buffer中位数约511.5MiB，未观测到writer_waiting。

对本轮与此前逐秒scaling_cur_freq记录，两核合并中位数：WritingData约2.60GHz vs3.30GHz，Verifying约2.10GHz vs2.80GHz。该频率样本含idle核心，不是活动线程有效频率或温控归因证据；足以说明没有控制成严格等价环境。不能用本轮verify235.71秒与前轮串行179.43秒直接判断回退或提升。核心性能结论来自上述同文件AB/BA。

校验时额外一次1秒pidstat采样：owner约6%单核，tapecpy-verify约61%单核；表现为读设备与计算在不同线程进行。持续阶段统计含定位空闲，不把这次短采样当作长期CPU均值。

## 构建、部署与证据

Rust1.97.1，基于63f7e885001ab5ff8d050da1b5dd1d81078bb794工作树增量，release/LTO默认配置，无target-cpu=native。生产二进制SHA256：933a690a00fb3673e2dd1a279616aff9d098f86a611740ea67bac6e1c5f51b5e。正式部署/home/ain/tapecpy-g3260-20261025/bin/tapecpy，~/.local/bin入口不变。旧版备份/home/ain/tapecpy-verify-pipeline-20261025/tapecpy.before（SHA a0506270e934f340d5699f8d27dc9c65662db9b1904dc32fafe0c4bc85d5254d）。

AB/BA测试构建SHA256：67455a462359c225999720c5ed3ed6a6e399bfa4d43c61af0212d8420c607061；该构建早于最后增加的测试用例/等待断言，但生产管线和AB/BA代码相同，不与正式release混淆。

远端/home/ain/tapecpy-verify-pipeline-20261025/ab.log、production-write.log及bench；正式job原始数据/home/ain/tapecpy-g3260-20261025/results/nfs-tape-20261025T124914Z。本地同名JSON保存四轮AB/BA和正式任务汇总；完整正式job临时归档/tmp/tapecpy-verify-production.tar。

本轮改动已实现并部署，正确性验收通过、只读对照显示约24%改善。距离140MB/s的差距仍需在固定频率并增加长流计时后研究；本次不擅自调整CPU调速、电压或磁带机配置。
