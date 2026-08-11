//! tapecpy 核心库。
//!
//! 分层关系（依赖方向自顶向下）：
//!
//! ```text
//! Presentation (CLI/TUI)
//!      ↓
//! app       —— 用户操作与工作流
//!      ↓
//! ltfs      —— LTFS 格式逻辑（label/index，纯数据，可脱离磁带测试）
//!      ↓
//! device    —— Linux 磁带设备访问（st / sg / sysfs）
//! ```
//!
//! 当前为 Milestone 0-2：设备发现、介质检查与 LTFS 识别。

pub mod app;
pub mod device;
pub mod job;
pub mod ltfs;
pub mod tui;
