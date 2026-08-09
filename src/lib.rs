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
//! 当前为 Milestone 0：设备发现与身份显示。

pub mod app;
pub mod device;

