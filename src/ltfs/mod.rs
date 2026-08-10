//! LTFS 格式层（与设备 I/O 分离的纯数据逻辑）。
//!
//! 本层不访问磁带设备，也不依赖 TUI/CLI：
//! - `label`：ANSI VOL1 label 与 LTFS XML label 的解析；
//! - `index`：LTFS XML index 的解析与摘要；
//! - `scan`：从磁带记录流中定位最新 index 的纯逻辑。
//!
//! 设备层通过 record 流与本层交互：本层只消费字节，不关心这些字节来自
//! 真实磁带还是测试 fixture。

pub mod index;
pub mod label;
pub mod mam;
pub mod scan;
