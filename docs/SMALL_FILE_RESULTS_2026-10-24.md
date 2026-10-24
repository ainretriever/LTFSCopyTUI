# 小文件开销实测与容量修复（2026-10-24）

## 容量修复与部署

CLI、TUI 和 runner 统一按有效 LTFS label 的 data_partition 显式查询 MAM 0000h，验证角色、分区和属性长度。失败为 Unknown，不回退索引分区，保留90% warning和容量不足拦截。
实机装载后 index p0=36,476MiB、data p1=1,279,001MiB，100GiB预检Normal；这是追加本轮夹具之前的容量采样。未装载时返回Unknown，LOAD后成功。
正式程序已部署 .125，入口 ~/.local/bin/tapecpy，旧版备份 tapecpy.before-capacity-fix-20261024。SHA-256：`c28753240393f7ad5bd8b8047d1b4dff11e4e7f81caf0562ed4fa1cb8ea04b50`。
175项回归通过、4项显式性能测试默认忽略；Clippy all-targets/all-features -D warnings及release构建通过。

## 主机真实路径矩阵

4KiB×10000、64KiB×2048、1MiB×256，每种平铺/分层两布局，交替协议顺序重复3次。共54次源、54次目标测量及12次数据生成，全部校验通过。源测真实SourcePipeline接空sink，包含SHA、扫描单列。目标测真实ReadOutputFiles安全prepare/open/finish/sync方法，模拟输入预生成及事后内容验证不计时；不包含destination消息队列开销。
未清空客户端/服务端缓存：例如NFS 1MiB源速度超过网卡带宽，表明客户端缓存命中，不能理解为网络吞吐。没有服务器冷盘结论。
以下均为3次中位数。目标data不含准备；目标total包含prepare+data+sync。

| 布局 | 文件大小 | 路径 | 源data MB/s | 目标prepare s | 目标data MB/s | 目标total MB/s |
|---|---:|---|---:|---:|---:|---:|
| 平铺 | 4KiB | local | 201.37 | 0.105 | 1258.47 | 223.28 |
| 平铺 | 4KiB | nfs | 18.56 | 13.471 | 4.47 | 1.81 |
| 平铺 | 4KiB | smb | 6.16 | 27.052 | 3.54 | 1.06 |
| 平铺 | 64KiB | local | 1118.34 | 0.033 | 3520.52 | 764.64 |
| 平铺 | 64KiB | nfs | 991.51 | 2.760 | 62.34 | 27.31 |
| 平铺 | 64KiB | smb | 75.69 | 1.941 | 53.94 | 30.30 |
| 平铺 | 1024KiB | local | 1513.60 | 0.021 | 3494.48 | 874.50 |
| 平铺 | 1024KiB | nfs | 1412.12 | 0.355 | 200.59 | 158.02 |
| 平铺 | 1024KiB | smb | 163.93 | 0.128 | 231.44 | 208.38 |
| 分层 | 4KiB | local | 208.69 | 0.077 | 1248.99 | 263.77 |
| 分层 | 4KiB | nfs | 18.63 | 13.432 | 4.46 | 1.81 |
| 分层 | 4KiB | smb | 3.46 | 4.944 | 3.37 | 2.39 |
| 分层 | 64KiB | local | 1177.68 | 0.024 | 3459.97 | 816.83 |
| 分层 | 64KiB | nfs | 496.51 | 2.835 | 64.05 | 26.95 |
| 分层 | 64KiB | smb | 72.99 | 0.917 | 118.75 | 65.40 |
| 分层 | 1024KiB | local | 1552.12 | 0.016 | 3663.40 | 899.91 |
| 分层 | 1024KiB | nfs | 1488.46 | 0.355 | 200.58 | 158.46 |
| 分层 | 1024KiB | smb | 164.37 | 0.120 | 232.89 | 210.85 |

## 并发主机探针

独立Python open/read/hash/close探针，不是正式Rust多线程预取。4KiB×10000，两布局、3次、1/4/16workers，共54项，内容全部匹配。平铺中位数：

| 路径 | 1 worker MB/s | 4 workers | 16 workers |
|---|---:|---:|---:|
| local | 623.81 | 194.97 | 138.74 |
| nfs | 20.88 | 71.46 | 88.84 |
| smb | 6.62 | 11.60 | 14.74 |

本地并发反而变慢，说明增加线程不是通用优化；Python调度和缓存也影响结果。NAS并发可改善逐文件访问，但不能保证正式管线得到相同增益。

## 磁带基础路径

内存伪随机源，经正常LTFS writer追加 /record-benchmark-20261024。4KiB/64KiB/512KiB各3组、每组64MiB，交替顺序，共576MiB、52,608文件，一次提交generation 9。文件真实extent、SHA进入index，原文件保留。直接读取新增连续records并逐文件SHA校验，全部通过。
测试专用SG_IO钩子不进入正式程序。下表仅为ioctl命令延迟，排除缓冲清零、哈希、管线空隙、定位和index；缓存/预读会影响结果，不能作为稳态物理吞吐。

| 操作 | record | 命令数 | 中位us | 均值us | P99us | 最大ms |
|---|---:|---:|---:|---:|---:|---:|
| read | 4KiB | 49152 | 155.81 | 157.29 | 169.50 | 6.465 |
| read | 64KiB | 3072 | 288.92 | 335.46 | 557.04 | 15.561 |
| read | 512KiB | 384 | 1406.90 | 1449.46 | 1653.47 | 3.331 |
| write | 4KiB | 49152 | 65.64 | 116.19 | 79.35 | 2406.900 |
| write | 64KiB | 3072 | 205.99 | 311.73 | 2573.79 | 15.513 |
| write | 512KiB | 384 | 1519.79 | 15153.99 | 38121.40 | 4549.006 |

4KiB读API均值177.66us，SG_IO均值157.29us，缓冲初始化和响应处理等额外约20.37us/record。144MB/s的4KiB流需要约35,156条/s，即平均28.44us/条，当前同步小record路径达不到这一目标。
首次WRITE等待2.407s；另一个512KiB WRITE等待4.549s，发生于第2轮512KiB分组。各组共享驱动器缓存且没有独立flush，前一段小record会影响后一组。未采物理backhitch计数，不能将停顿定性为介质故障，也不能用512KiB组均值推翻此前大文件144MB/s稳态结果。
一次温度采样35°C，BER worst=0.0不是有效负数错误率，不能据此宣布物理通道无错。此基准只输出温度/worst，未输出完整逐通道窗口；正式job仍保存channel_counter_window。

## 系统调用与协议计数

另外新建1000个4KiB目标进行strace -f -c -w；追踪运行不混入性能样本，包含事后验证。各路径1745次openat2（根1+create1000+重开744，256个handle保留），1次syncfs，没有逐文件fsync。NAS的openat2/close显著慢于本地；verification的openat/read也包含在追踪总数中。
NFS mountstats差值OPEN=1000、GETATTR=1000、WRITE=1000，READ=0、COMMIT=0，独立验证命中缓存。SMB有缓存和延迟关闭，after仍显示2000个缓存open handle。用户态open/close次数不等于wire CREATE/CLOSE次数，不能把重复open都算成网络往返。原始表和协议计数见JSON。

## 判断及下一步

1. 当前4KiB网络源读取和目标创建/写入都有独立主机瓶颈，与此前完整任务低速相符。下一步应研究受限并发NAS预取和目标文件操作调度。
2. 磁带同步小record命令成本也是独立限制。改善NAS仍不等于小文件满速；之后若要逼近大文件吞吐，需要设计符合LTFS的跨文件record packing，保留byteoffset、hash、取消与失败语义。
3. 本轮只修容量及容量显示，未改变正式小文件数据路径、记录布局、挂载参数或同步语义。可以继续大批量测试。

## 最终状态与证据

最终Healthy，safe_for_normal_write=true。p0b5/p1b296524两份index和两份VCI均generation 9、UUID一致，index约76.17MB。测试目录保留，因此卷元数据比测试前更大。最终health未报告uncorrected读写错误或TapeAlert；read without-delay=340，不能说所有物理计数为零。
磁带保持装载，无遗留测试进程。远端原始目录 /home/ain/tapecpy-small-20261024/；主机原始样本、完整统计、二进制hash、strace/协议计数及诊断见 [JSON](SMALL_FILE_RESULTS_2026-10-24.json)。SG_IO原始大日志保留远端，其SHA-256在JSON中。
脚本初次SMB目标因与NFS共享后端重名而被create-new拒绝，无覆盖。修正独立目录后按完成tag继续，失败日志保留。
