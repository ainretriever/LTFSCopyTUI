//! tapecpy 核心库。
//!
//! 分层关系（依赖方向自顶向下）：
//!
//! ```text
//! Presentation (CLI/TUI)
//!      ↓
//! app       —— 用户操作与工作流
//!      ↓
//! device    —— Linux 磁带设备访问（st / sg / sysfs）
//! ```
//!
//! 当前为 Milestone 0/1：设备发现、身份显示与介质检查。

pub mod app;
pub mod device;
