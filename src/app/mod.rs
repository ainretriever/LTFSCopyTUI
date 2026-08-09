//! Application 层：用户操作与工作流编排。
//!
//! Milestone 0/1 包含设备发现、选择与介质检查。后续的 LTFS 格式化、
//! 写入等工作流都从这里编排，Presentation 层不得直接操作设备。

use std::path::Path;

use crate::device::{self, TapeDrive};

/// 发现当前系统上的全部磁带机。
pub fn discover_drives() -> Result<Vec<TapeDrive>, device::Error> {
    device::discover()
}

/// 根据选择器挑选一台磁带机。
///
/// `selector` 可以是 1 基序号、`/dev/nstX`、`/dev/stX` 或 `/dev/sgX`。
/// 不提供选择器且系统只有一台磁带机时，直接返回这一台。
pub fn select_drive<'a>(
    drives: &'a [TapeDrive],
    selector: Option<&str>,
) -> Result<&'a TapeDrive, String> {
    let drive: &'a TapeDrive = match selector {
        None => match drives {
            [one] => one,
            [] => return Err("系统上未发现磁带机。".into()),
            _ => {
                return Err(
                    "系统上有多台磁带机，请用 `tapecpy info <选择器>` 指定一台。".into(),
                )
            }
        },
        Some(sel) => {
            if let Ok(idx) = sel.parse::<usize>() {
                drives
                    .get(idx.checked_sub(1).unwrap_or(usize::MAX))
                    .ok_or_else(|| format!("序号 {idx} 超出范围（共 {} 台）。", drives.len()))?
            } else {
                drives
                    .iter()
                    .find(|d| {
                        let p = Path::new(sel);
                        p == d.nst_path || p == d.st_path || p == d.sg_path
                    })
                    .ok_or_else(|| format!("找不到设备 `{sel}`。"))?
            }
        }
    };
    Ok(drive)
}

/// 检查一台磁带机的介质状态与基本信息（Milestone 1）。
pub fn inspect_media(drive: &TapeDrive) -> Result<device::MediaInfo, device::Error> {
    device::inspect_media(drive)
}
