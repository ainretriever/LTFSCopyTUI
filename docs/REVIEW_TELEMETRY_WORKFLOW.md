# 写入遥测与 LOG SENSE 审阅记录（2026-08-10）

## 1. 范围与分层

本里程碑首先实现不改变磁带位置的健康快照：

- `device::scsi`：发送 LOG SENSE(10)，PC=1（cumulative values）；
- `device::tape`：同一 `TapeSession` 先读 4-byte header，再按 page length 读取；
- `device::log`：纯解析 page header、变长 parameter 和大端无符号值；
- `app`：组合健康快照，并计算写入会话 baseline/final 差值；
- CLI：`tapecpy health` 展示原始累计值，`tapecpy write` 展示本次差值。

遥测读取失败只产生结构化 warning，不应令原本可完成的 LTFS 写入失败。

## 2. LTFSCopyGUI 对照

LTFSCopyGUI `TapeUtils.LogSense` 同样先以 allocation length 4 读取 header，再按
page length 重读，并默认使用 page control 1。其页面定义使用：

- 02h：Write Error Counters；
- 03h：Read Error Counters；
- 2Eh：TapeAlert；
- error parameter 03h：total corrected；
- error parameter 06h：total uncorrected。

tapecpy 只借鉴命令和字段语义，没有引入其 WinForms page descriptor 结构。

### 2.1 社区所称的“通道错误率”

LTFSCopyGUI 的通道错误率并非 LOG SENSE 02h/03h 的 corrected error 比率，而是
通过 RECEIVE DIAGNOSTIC RESULTS(6) 读取厂商 ASCII 页面：

- 写通道 page 88h；
- 读通道 page 87h；
- 每通道五项十六进制累计值：C1 errors、C1 uncorrectable、header errors、
  write-pass errors、CCPs。

两次采样间每通道使用原算法：

```text
log10((C1_after - C1_before) / (CCP_after - CCP_before) / 2 / 1920)
```

兼容行为也保持一致：CCP 没有前进的通道用 `-2.98` 占位；只要其他通道有
有效数据，该占位会参与最差值计算；最终值低于 `-10`（含零 C1 形成的负无穷）
时总结果归零。这些规则不重新解释，以保证输出能直接与 LTFSCopyGUI 和玩家社区
既有数据比较。

## 3. 真实设备结果

Quantum ULTRIUM 5 固件 3210 支持以上三个标准页面。测试时累计值为：

```text
write: corrected=0, uncorrected=0, processed=6
read:  corrected=0, uncorrected=0, processed=168
       corrected-without-delay=117, correction-runs=117
TapeAlert: none
```

这证明累计计数不能直接称作“本次任务错误”。随后在 generation 3 卷写入
19-byte `/telemetry-m10.txt`，generation 更新为 4，写入前后差值为：

```text
corrected-write=0 hard-write=0 corrected-read=0 hard-read=0
```

文件 SHA-256、两个 index 和 MAM VCI 均正常完成。

该固件也支持 diagnostic 87h/88h。写入 16 MiB 不可压缩随机数据后，88h 的
16 个通道均产生有效差值，按 LTFSCopyGUI 原算法得到：

```text
ch00 -6.11  ch01 -5.60  ch02 -6.11  ch03 -5.76
ch04 -6.24  ch05 -5.93  ch06 -6.01  ch07 -6.41
ch08 -5.76  ch09 -5.28  ch10 -5.31  ch11 -5.81
ch12 -6.71  ch13 -5.60  ch14 -6.71  ch15 -5.57
worst: -5.28
```

卷正常提交 generation 5，标准 LOG SENSE 本次 corrected/hard error 差值仍为 0。

## 4. 当前边界

- 周期 sample 同时包含区间实时吞吐、通道错误率、时间和磁带位置；
- 尚未给 TapeAlert flag 编号附加标准文本和严重级别；
- 未读取 Volume Statistics 17h、Sequential Access 0Ch 或厂商页面；
- 计数器下降视为 reset/unknown，不伪造差值；
- 中途失败目前无法返回完整 final snapshot，需要在错误/取消模型中统一解决。

## 5. 采样与历史策略

LTFSCopyGUI 的 capacity/channel refresh 默认目标间隔是 5 秒，但实际依赖写入
流程到达允许刷新位置；其 21,600 点（6 小时）数组用于每秒速度/文件速率曲线。
虽然还声明了 `ErrRateLog` 数组，当前源码没有向它追加通道错误率时间序列，
只保留最新的各通道值和最新最差值。

tapecpy 不继承无实际用途的 6 小时通道历史，确定为：

```text
目标采样间隔       5 秒
滚动历史容量      10 分钟 / 120 samples
默认显示窗口       5 分钟 / 60 samples
会话摘要           全程最差值 + channel + timestamp + tape position
```

滚动窗口用于观察近期趋势；会话摘要用于任务完成报告。周期查询仍必须由统一设备
所有者在安全命令边界执行，5 秒是目标间隔而不是允许另一个线程抢占设备的承诺。
每个成功 sample 的实时吞吐按它与上一个成功 sample 之间的有效载荷字节差除以
实际时间差计算，供 TUI 与通道错误率共用时间轴显示；不采集会话平均速度。

## 6. 流式测试源与完整历史验证

为避免准备约 80 GiB 本地测试文件，Application 层增加有界、确定性的
SplitMix64 little-endian 测试源：

```text
tapecpy write-random 80GiB /random-stream-80g.bin --seed=2026081001
```

它直接填充正常 LTFS writer 的 buffer，不落盘、不绕过 extent/index/VCI 和
SHA-256。生成器不用于密码学；用途是产生难压缩、可由相同 seed 和长度重现的
吞吐测试数据。新写入会把 algorithm、seed、size 保存为文件 xattr。

2 GiB 预检得到 5 个周期样本，position 从 p1b57 单调前进到 p1b3700，卷正常
提交 generation 6。随后完整写入 80 GiB：

```text
size:             85,899,345,920 bytes
seed:             2026081001
data duration:    about 805 seconds
observed payload: about 100--110 MiB/s
history retained: 120 samples
default visible:  60 samples
session worst:    -4.44, channel 15, t=740.4s, p1b153608
SHA-256:          a0006ca0bd4fb15665884184f94a6fcb00175babcd16c57a3d056f9e2ff4d5b9
generation:       6 -> 7
VCI:              a:5 / b:167996, generation 7
```

运行超过 10 分钟后历史保持 120 点，证明旧样本被滚动淘汰；先前最差摘要不会
随历史淘汰而消失。所有采样点的 logical position 单调前进，标准 LOG SENSE
corrected/hard read-write error 差值均为 0。

本次测试也观察到当前单线程“生成 + SHA-256 + WRITE”约为 100–110 MiB/s，低于
LTO-5 140 MB/s 标称原生速率。流式生成器只用于测试；实际工作流将从 NFS 或
SMB 挂载目录读取真实文件，因此不以优化测试生成器作为后续任务。

修正生成器使输出不受 `Read` 调用分块大小影响后，又以 seed 42 写入 1 MiB
样本并提交 generation 8。OpenLTFS 2.4.8.4 直接挂载后确认：

```text
user.tapecpy.test.pseudorandom.algorithm="splitmix64-le"
user.tapecpy.test.pseudorandom.seed="42"
user.tapecpy.test.pseudorandom.size="1048576"
user.ltfs.hash.sha256sum="5b2605c7135a3f8c54d75039514f0bcb798cfe1a8d74f57380d45aaadea36dca"
```

OpenLTFS 识别 index chain `(a,5) -> (b,168001)`，随后安全卸载。
