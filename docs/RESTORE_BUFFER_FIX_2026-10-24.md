# 恢复有效缓冲与等待指标修复（2026-10-24）

## 实现

修复位于app的host-output管线，保持一个磁带owner和一个目标writer。此前每条数据按最大1MiB record分配槽，ExtentStart/ExtentEnd也占固定容量通道的槽，导致512MiB名义预算只能容纳极少小文件payload。snapshot按数据消息数推断满载，且observer只在发送/读取继续后调用，未反映控制消息造成的阻塞。

现在采用可复用的1MiB主机payload块，每块另有有界的extent/数据范围描述符，按原始顺序跨文件聚合；这不是把多个文件写进同一个磁带record，磁带格式和读取顺序没有变化。reader复用独立的record工作缓冲，把选中的数据复制到块中，代价是一次额外payload复制。每块最多4096条描述符，满payload或描述符即提交，Finish以及已知非连续LOCATE前也提交；不在每个小文件结束时强制提交半空块。

默认最多512块，按需分配，在producer/队列/writer/回收池之间转移，不额外保留第二套池。内存通道采用mpsc channel，但只有先取得有限块的单一producer能提交，因此消息数量有严格的所有权上限。payload预算512MiB之外，还有有界描述符数组、reader工作缓冲/共享record缓存、队列节点，以及既有计划、文件句柄和index内存；不是512MiB进程RSS承诺。描述符按需增长，上限512×4096×size_of(ReadBatchEntry)。极小payload/纯控制流或频繁定位导致提前提交仍可能先触及描述符/块数限制；used_bytes表示未写出的payload，整批回收前已写出的空间暂时也不能复用。

writer仍按每个最终extent逐文件sync_all并检查close，结束时同步目录/根目录/syncfs，没有降低上一轮同步保证。

等待指标改为真实的块回收等待：立即发布进入/退出状态、每100ms发布等待快照并检查取消，记录累计次数和纳秒时长。只有设备owner调用observer，不从目标线程访问设备，也不为等待发布增加SCSI命令。job对等待状态变化绕过250ms节流；TUI显示queue full与累计Wait秒/次数。累计值完成后保留，旧状态缺字段默认0。持续等待/末尾排空期间继续做1秒吞吐采样，避免停读时一直保留旧速度；排空期也发布实际写出字节和已同步文件数。

取消可以打断缓冲等待；正在执行的目标filesystem同步/写入等系统调用仍遵循既有的返回边界，不声称能强制中断hard NFS等阻塞I/O。writer失败通过断开的回收通道唤醒producer，任务仍返回真实writer错误。

## 测量方法

.125 Quantum LTO5/SAS2308，2.5GbE；.239 NVMe ext4/LVM共享，NFS4.2、SMB3.1.1 cache=strict。保持挂载、缓存和单writer不变。before是已修复逐文件同步的版本，不能与旧弱同步版本混比。

- 8项默认512MiB容量探针：4种size×before/after。暂停真实输出worker直到producer背压后取消；验证容量、阻塞与取消，不宣称验证文件内容。新版使用一个文件的多个extent制造同等控制消息比例，避免预建超过13万文件；旧版为原4096文件探针。worker不消费数据时，文件编号/偏移不影响队列容量。暂停/取样窗口不同，不比较耗时。
- 24项真实host-output：3目标×4KiB/1MiB×2版本×两轮AB/BA。4KiB×1000和1MiB×1024，每项逐文件SHA复核在计时外。总时间包括验证、prepare、提交、排空、同步；不是磁带测速。未清缓存。
- 一次既有8GiB/8192文件夹具的磁带→SMB恢复，磁带只读；内容SHA、FLUSH和READ/LOCATE计数另行记录。

原始行、CIFS before/after和进程峰值RSS见 [JSON](RESTORE_BUFFER_FIX_2026-10-24.json)，复现脚本为 `scripts/network_test/test-restore-buffer.py`，远端目录为 `/home/ain/tapecpy-buffer-fix-20261024`。测试构建的allocated_buffers现在表示record工作缓冲数，新增allocated_payload_slabs表示payload块数，两者不能与旧版单一buffer数量直接比较；queued_records表示数据片段数，也不等于设备READ数量。

## 默认容量探针

|文件大小|record|原有效payload|修复后payload|修复后块数|修复后峰值RSS MiB|
|---|---|---:|---:|---:|---:|
|4KiB|4KiB|688KiB|512MiB|512|539.07|
|64KiB|64KiB|10.75MiB|512MiB|512|520.39|
|1MiB|512KiB|129MiB|512MiB|512|519.52|
|4MiB|512KiB|206MiB|512MiB|512|519.45|

所有暂停窗口均捕获waiting=true，取消断言通过。新版record工作缓冲只分配1个。RSS差异也包含两版探针计划形状的不同，不能把峰值差异全部归为队列本身；主要验收指标是实际payload和严格块数量上限。默认512MiB在约134MB/s读带且目标完全停止时，理论上约能提供4秒缓冲；这是容量换算，并非实测物理连续走带时长。

## 主机输出对照

两轮总秒数中位数，样本很少，不作显著性结论。

|目标|文件大小/数量|修复前秒|修复后秒|变化|
|---|---|---:|---:|---:|
|local|4KiB×1000|0.9973|0.9963|-0.1%|
|NFS|4KiB×1000|2.1665|2.1613|-0.2%|
|SMB|4KiB×1000|1.7941|1.8245|+1.7%|
|local|1MiB×1024|2.1918|1.9899|-9.2%|
|NFS|1MiB×1024|6.9034|6.8479|-0.8%|
|SMB|1MiB×1024|7.2784|7.2319|-0.6%|

每项内容SHA一致。网络总耗时没有显示明显变化；本地1MiB两轮存在波动，不能宣称稳定9%优化。本次主要改善短时抖动的吸收能力和等待可观察性，不解决目标持续吞吐不足、逐文件fsync延迟或预建目录成本。

## 回归检查

194项通过，10项需显式设备/性能条件的测试保持ignored；Clippy all-targets/all-features -D warnings通过，正式release构建通过。新增5项回归覆盖1–512KiB片段的预算充满、阻塞中反复发布等待/取消、解除背压后复用块、描述符上限、跨块/文件边界内容与顺序、writer故障唤醒阻塞并保留实际错误。既有job指标测试补充累计值、完成保留、旧状态反序列化缺省验证。前一轮同步失败回归仍通过。

## 真实磁带 → SMB

恢复既有generation12夹具 `/restore-research-20261024/size-1048576`：8GiB、8192个1MiB文件，全部SHA匹配，16384次READ、一次数据LOCATE；测试未写磁带，输出校验后删除。

|指标|上一轮同步修复版|本次缓冲版|
|---|---:|---:|
|完整8GiB读带跨度秒|64.217|64.285|
|读带MB/s（十进制）|133.77|133.62|
|file_sync次数|8192|8192|
|file_sync累计秒|42.236|42.558|
|READ间隙P99 us|16.828|174.041|
|READ间隙累计秒|0.330|1.940|

额外复制确实增加了READ之间的主机处理间隙，不能称为零开销；间隙还包括其他producer/observer工作，不能把差值全部精确归为memcpy。当前LTO5实测吞吐变化约−0.11%，单轮对照没有可辨识的持续吞吐回退；不能推广为其他CPU/LTO代际的零成本。

本次计划40.907秒独立于恢复计时；恢复203.944秒包含18.461秒prepare、119.423秒首次数据LOCATE。SG_IO最长一次4.371秒，仍在sequence3351，此设备长READ现象并未由缓冲修复消除，也没有证据证明是目标背压造成。

0.25秒样本最大queued payload为3MiB，没有捕获tape_waiting=true；它是采样结果，不是瞬时绝对峰值或物理倒带次数测量。SMB FLUSH从20337增至28529，增量8192、失败增量0；8项SMB主机对照在两版中也均保留每文件一次FLUSH，没有靠降低同步强度换容量。最终health读/写uncorrected均0，无活动TapeAlert；其他纠错累计字段非全零，完整文本在JSON。

## 部署与构建来源

正式release已部署到 .125 `/home/ain/tapecpy-perf-20261023/bin/tapecpy`，`~/.local/bin/tapecpy`仍指向该文件，帮助命令和health查询成功。旧版本同目录备份 `tapecpy.before-buffer-fix-20261024`。

- 正式release SHA256：`2ffaeb61b5e7897788f233d173da1a3c829f9590e5f2485b00ac6ee52d06eecb`。
- 旧正式release SHA256：`3880f32c2e39c8452653fd9e5b41075406e7bad520ba198c9cf3530e8cfdb85a`。
- before测试构建：`72dac1151bb2f2493f47516cb9bc48eef32f98f713227df612c7d918e7f4a813`。
- after测试构建：`0b952d9d73de51cf0e6ea2da8f18d052e86fe73cdee877915b49553aa7c75c96`。

远端测量过程中，最终代码补齐了等待observer的一秒吞吐刷新、等待定位前使用已知设备位置以及TUI等待措辞；payload块、描述符、复制、取消及writer/sync算法与after测量构建相同。最终release包含这些observer/UI修正，本地完整回归/Clippy覆盖最终逻辑；不能把测试二进制SHA与正式无测试构建SHA混同。
