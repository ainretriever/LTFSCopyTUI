# SHA256长流对照（2026-10-26，后台执行中）

用户提供FA001AL5（MAM barcode FA001A），HP介质9130804161，写保护开启。卷名furaffinity backup 01A，UUID d3eca6c1-f138-4fd4-9fdc-a51e76304551，generation2，20文件，总计1021522397184字节，全部有SHA256。预检health无活动TapeAlert、读写corrected/uncorrected均0。

选择/tape01/00.tar，93362483200字节，单extent，索引SHA256 e6f668f4cc5249b6e759eba4c6314355420de56179769d15d6a8068d31e99ddf。此前短4GiB管线测试不足以推断长流极限，本次使用整个约93GB文件。

## 顺序与指标

相同文件依次read、serial、pipeline、pipeline、serial、read。read仅读取丢弃payload，不声称完整性已验证；serial/pipeline四轮均检查原SHA256，不匹配则测试停止并保留错误。唯一设备lease贯穿整批，不写磁带、不保存大文件payload、不修改CPU governor/频率。

测试构建内记录buffer令牌耗尽等待次数/秒数（pool_wait）、READ之间的总间隙/最大间隙/超过1ms次数、READ调用累计时间、首次READ至hash完成时间、每秒应用payload吞吐。该计时排除首次LOCATE，wall另报含定位时间；每轮文件数据相同。指标仅cfg(test)，生产二进制未替换。采样和每秒日志会有少量观测开销。

外部只读采样器每秒保存各线程/proc stat及两核scaling_cur_freq，启动时保存CPU与governor。频率未锁定，不把idle核心频率当作活动线程有效频率。应结合等待、间隙、吞吐、频率来解释差异，不直接把缺少SHA-NI当成唯一根因。稳定区间可在每轮时间轴裁去首尾至少10秒后计算；完整段结果也需保留。

## 软件检查及启动

共享校验worker6项专项通过，all-targets/all-features Clippy -D warnings通过，rustfmt和git diff --check通过；release测试构建成功。不自动运行其他ignored硬件测试。

测试二进制SHA256 57142674df3c13e1d2c48e477c8bd985ed14e6bc8bc20d322fbd40601822ee33，远端/home/ain/tapecpy-sha256-long-20261026/bench。驱动scripts/g3260_test/run_long_verify.py；精确测试app::verify_pipeline::hardware::verify_ab，TAPECPY_VERIFY_AB携带夹具及modes参数。

后台driver PID1888；长流数据量六轮约560GB，预计需较长时间。本轮不持续守候，不提前宣称吞吐或完整性通过。

远端/home/ain/tapecpy-sha256-long-20261026/long-comparison/：status.json为运行状态，test.log含VERIFY_STREAM_SAMPLE与VERIFY_AB_RESULT，host-samples.jsonl记录线程和频率，results.json在结束后保存结果；health-after.txt在释放lease后生成。driver.log在父目录。库存清单index-inventory与media/health-before亦在父目录。保持磁带装载，不并行发其他磁带命令。

## 已完成

六轮均正常结束，四轮SHA256均通过。管线140.97MB/s，仅读取141.00MB/s，串行116.20MB/s；管线buffer等待0次。详见SHA256_LONG_STREAM_RESULTS_2026-10-26.md，前文“执行中”为启动时历史。
