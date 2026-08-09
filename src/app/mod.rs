//! Application 层：用户操作与工作流编排。
//!
//! Milestone 0/1 包含设备发现、选择与介质检查。后续的 LTFS 格式化、
//! 写入等工作流都从这里编排，Presentation 层不得直接操作设备。

use std::path::Path;

use crate::device::{self, TapeDrive};
use crate::device::tape::{ReadRecord, TapeSession};
use crate::ltfs;
use crate::ltfs::label::{AnsiLabel, Label};
use crate::ltfs::index::Index;
use crate::ltfs::scan::ScanRecord;

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

/// LTFS 卷检查结果（Milestone 2）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeInfo {
    /// 是否识别出有效的 LTFS label。
    pub recognized: bool,
    /// 未识别时的原因说明。
    pub reason: Option<String>,
    /// 非致命警告（分区探测失败、label 损坏等）。
    pub warnings: Vec<String>,
    /// ANSI VOL1 label 中的卷序列（barcode）。
    pub ansi_barcode: Option<String>,
    /// 解析出的 LTFS XML label。
    pub label: Option<Label>,
    /// 最新解析成功的 index 摘要。
    pub index: Option<Index>,
}

/// 探测一个物理分区的 label（Milestone 2）。
///
/// 分区布局：`[VOL1 记录][FileMark][XML label 记录][FileMark]`。
/// 返回 `None` 表示该分区没有有效 LTFS label。
fn probe_partition_label(
    session: &mut TapeSession,
    partition: u8,
) -> Result<Option<(AnsiLabel, Label)>, device::Error> {
    session.locate(partition, 0)?;

    let ansi = match session.read_record()? {
        ReadRecord::Data(buf) => match AnsiLabel::parse(&buf) {
            Some(label) => label,
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    if !matches!(session.read_record()?, ReadRecord::Filemark) {
        return Ok(None);
    }
    let xml = match session.read_record()? {
        ReadRecord::Data(buf) => buf,
        _ => return Ok(None),
    };
    let xml = match String::from_utf8(xml) {
        Ok(xml) => xml,
        Err(_) => return Ok(None),
    };
    let label = match Label::parse_xml(&xml) {
        Ok(label) => label,
        Err(_) => return Ok(None),
    };
    if !matches!(session.read_record()?, ReadRecord::Filemark) {
        return Ok(None);
    }
    Ok(Some((ansi, label)))
}

/// 在指定物理分区上从 label 之后扫描最新 index。
fn scan_latest_index(
    session: &mut TapeSession,
    partition: u8,
) -> Result<Option<Index>, device::Error> {
    // label 结束于块 3（VOL1, FM, XML label, FM；某些实现后跟一个多余 FM），
    // index 从块 4 开始。
    session.locate(partition, 4)?;

    // 先快速定位 EOD（避免逐块读到 blank check，LTO 驱动器上这一步很慢）。
    let eod_block = session.space_to_eod()?;
    if eod_block <= 4 {
        return Ok(None);
    }
    session.locate(partition, 4)?;

    let mut records = Vec::new();
    loop {
        match session.read_record()? {
            ReadRecord::Data(buf) => records.push(ScanRecord::Data(buf)),
            ReadRecord::Filemark => records.push(ScanRecord::Filemark),
            ReadRecord::Eod => {
                records.push(ScanRecord::Eod);
                break;
            }
        }
        // 到达已知 EOD 块即停止，不再读最后一条（blank check 很慢）。
        if session.read_position()?.block >= eod_block {
            records.push(ScanRecord::Eod);
            break;
        }
    }
    Ok(ltfs::scan::find_latest_index(records)
        .and_then(|xml| Index::parse_xml(&xml).ok()))
}

/// 检查一台磁带机上的 LTFS 卷（Milestone 2）。
///
/// 流程：读取两个物理分区的 label → 确定 index 分区 → 扫描最新 index。
pub fn inspect_volume(drive: &TapeDrive) -> Result<VolumeInfo, device::Error> {
    let mut session = TapeSession::open(&drive.sg_path)?;
    session.set_variable_block()?;

    let mut info = VolumeInfo::default();
    let mut labels: Vec<(u8, AnsiLabel, Label)> = Vec::new();

    // 先倒带到当前分区 BOT 并确认所在分区，优先探测当前分区，
    // 减少跨分区 LOCATE（LTO 驱动器上每次跨分区约 10 秒）。
    session.rewind()?;
    let start_partition = session.read_position()?.partition as u8;
    let order = [start_partition, 1 - start_partition];

    for &partition in &order {
        match probe_partition_label(&mut session, partition) {
            Ok(Some((ansi, label))) => {
                let is_index = label.this_partition == label.index_partition;
                labels.push((partition, ansi, label));
                // 该分区就是 index 分区时无需再探测另一个。
                if is_index {
                    break;
                }
            }
            Ok(None) => {}
            // 单分区磁带定位到分区 1 会失败，这里作为警告继续，不中断识别。
            Err(e) => info
                .warnings
                .push(format!("分区 {partition} 探测失败: {e}")),
        }
    }

    let Some((_, first_ansi, first_label)) = labels.first() else {
        info.recognized = false;
        info.reason = Some("两个分区均未找到有效 LTFS label（无 ANSI \"LTFS\" 签名）。".into());
        return Ok(info);
    };

    info.recognized = true;
    info.ansi_barcode = Some(first_ansi.barcode.clone());
    info.label = Some(first_label.clone());

    // 确定 index 分区的物理编号：label 中 this_partition == partitions.index
    let index_logical = first_label.index_partition;
    let index_phys: Vec<u8> = labels
        .iter()
        .find(|(_, _, l)| l.this_partition == index_logical)
        .map(|(p, _, _)| vec![*p])
        .unwrap_or_else(|| vec![start_partition]);

    for partition in index_phys {
        match scan_latest_index(&mut session, partition) {
            Ok(Some(idx)) => {
                if idx.self_location.partition == index_logical || info.index.is_none() {
                    info.index = Some(idx);
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => info
                .warnings
                .push(format!("分区 {partition} index 扫描失败: {e}")),
        }
    }

    if info.index.is_none() {
        info.warnings
            .push("已识别 LTFS label，但未找到可解析的 index。".into());
    }
    Ok(info)
}
