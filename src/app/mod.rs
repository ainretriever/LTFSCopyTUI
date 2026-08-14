//! Application 层：用户操作与工作流编排。
//!
//! Milestone 0/1 包含设备发现、选择与介质检查。后续的 LTFS 格式化、
//! 写入等工作流都从这里编排，Presentation 层不得直接操作设备。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::device::tape::{LongEraseStatus, MamAttributeFormat, ReadRecord, TapeSession};
use crate::device::{self, TapeDrive};
use crate::ltfs::index::{
    DirectoryEntry, ExtendedAttribute, Extent, FileEntry, FileTimes, Index, TapePos,
};
use crate::ltfs::label::{AnsiLabel, Label};
use crate::ltfs::mam::{self, ValueFormat, VolumeCoherencyInformation};

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
            _ => return Err("系统上有多台磁带机，请用 `tapecpy info <选择器>` 指定一台。".into()),
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

/// 不改变磁带位置的驱动器/介质健康快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriveHealth {
    pub write_errors: Option<device::log::ErrorCounters>,
    pub read_errors: Option<device::log::ErrorCounters>,
    pub tape_alerts: Vec<u16>,
    pub write_channels: Option<Vec<device::channel_error::ChannelCounters>>,
    pub read_channels: Option<Vec<device::channel_error::ChannelCounters>>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DriveHealthDelta {
    pub corrected_write_errors: Option<u64>,
    pub hard_write_errors: Option<u64>,
    pub corrected_read_errors: Option<u64>,
    pub hard_read_errors: Option<u64>,
    pub active_tape_alerts: Vec<u16>,
    pub write_channel_rates: Vec<device::channel_error::ChannelRate>,
    pub worst_write_channel_rate: Option<f64>,
}

impl DriveHealthDelta {
    fn between(before: &DriveHealth, after: &DriveHealth) -> Self {
        let write_channel_rates = before
            .write_channels
            .as_deref()
            .zip(after.write_channels.as_deref())
            .map_or_else(Vec::new, |(before, after)| {
                device::channel_error::rates(before, after)
            });
        let worst_write_channel_rate = device::channel_error::worst_rate(&write_channel_rates);
        Self {
            corrected_write_errors: counter_delta(
                before.write_errors.as_ref().and_then(|c| c.total_corrected),
                after.write_errors.as_ref().and_then(|c| c.total_corrected),
            ),
            hard_write_errors: counter_delta(
                before.write_errors.as_ref().and_then(|c| c.uncorrected),
                after.write_errors.as_ref().and_then(|c| c.uncorrected),
            ),
            corrected_read_errors: counter_delta(
                before.read_errors.as_ref().and_then(|c| c.total_corrected),
                after.read_errors.as_ref().and_then(|c| c.total_corrected),
            ),
            hard_read_errors: counter_delta(
                before.read_errors.as_ref().and_then(|c| c.uncorrected),
                after.read_errors.as_ref().and_then(|c| c.uncorrected),
            ),
            active_tape_alerts: after.tape_alerts.clone(),
            write_channel_rates,
            worst_write_channel_rate,
        }
    }
}

fn counter_delta(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    after?.checked_sub(before?)
}

pub fn read_drive_health(drive: &TapeDrive) -> Result<DriveHealth, String> {
    let mut session = TapeSession::open(&drive.sg_path).map_err(|e| e.to_string())?;
    Ok(read_drive_health_session(&mut session))
}

fn read_drive_health_session(session: &mut TapeSession) -> DriveHealth {
    let mut health = DriveHealth::default();
    for (page, kind) in [(0x02, "write error"), (0x03, "read error")] {
        match session.read_log_page(page, 0) {
            Ok(raw) => match device::log::parse_page(&raw, page) {
                Ok(parameters) => {
                    let counters = device::log::error_counters(&parameters);
                    if page == 0x02 {
                        health.write_errors = Some(counters);
                    } else {
                        health.read_errors = Some(counters);
                    }
                }
                Err(error) => health
                    .warnings
                    .push(format!("解析 {kind} LOG SENSE page 失败: {error:?}")),
            },
            Err(error) => health
                .warnings
                .push(format!("读取 {kind} LOG SENSE page 失败: {error}")),
        }
    }
    match session.read_log_page(0x2e, 0) {
        Ok(raw) => match device::log::parse_page(&raw, 0x2e) {
            Ok(parameters) => {
                health.tape_alerts = device::log::active_tape_alerts(&parameters);
            }
            Err(error) => health
                .warnings
                .push(format!("解析 TapeAlert LOG SENSE page 失败: {error:?}")),
        },
        Err(error) => health
            .warnings
            .push(format!("读取 TapeAlert LOG SENSE page 失败: {error}")),
    }
    for kind in [
        device::channel_error::PageKind::Write,
        device::channel_error::PageKind::Read,
    ] {
        match session.read_diagnostic_page(kind.page_code()) {
            Ok(raw) => match device::channel_error::parse_page(&raw, kind) {
                Ok(channels) => match kind {
                    device::channel_error::PageKind::Write => {
                        health.write_channels = Some(channels)
                    }
                    device::channel_error::PageKind::Read => health.read_channels = Some(channels),
                },
                Err(error) => health.warnings.push(error),
            },
            Err(error) => health.warnings.push(format!(
                "读取 channel diagnostic page 0x{:02x} 失败: {error}",
                kind.page_code()
            )),
        }
    }
    health
}

/// 检查一台磁带机的介质状态与基本信息（Milestone 1）。
pub fn inspect_media(drive: &TapeDrive) -> Result<device::MediaInfo, device::Error> {
    device::inspect_media(drive)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedFilesystem {
    pub mount_point: PathBuf,
    pub filesystem_type: String,
    pub source: String,
    pub network: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: BrowserEntryKind,
    pub size: Option<u64>,
}

/// Enumerate the host's already-mounted filesystems. tapecpy deliberately does
/// not mount NFS/CIFS shares itself; detached jobs reopen these Linux paths.
pub fn mounted_filesystems() -> Result<Vec<MountedFilesystem>, String> {
    let text = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| format!("读取 /proc/self/mountinfo 失败: {error}"))?;
    parse_mountinfo(&text)
}

pub fn selectable_filesystems() -> Result<Vec<MountedFilesystem>, String> {
    Ok(mounted_filesystems()?
        .into_iter()
        .filter(|mount| {
            mount.network
                || matches!(
                    mount.filesystem_type.as_str(),
                    "ext2"
                        | "ext3"
                        | "ext4"
                        | "xfs"
                        | "btrfs"
                        | "zfs"
                        | "vfat"
                        | "exfat"
                        | "ntfs"
                        | "ntfs3"
                        | "f2fs"
                )
                || mount.mount_point == Path::new("/tmp")
                || mount.mount_point.starts_with("/run/media")
                || mount.mount_point.starts_with("/mnt")
                || mount.mount_point.starts_with("/media")
        })
        .collect())
}

pub fn mounted_filesystem_for_path(path: &Path) -> Result<Option<MountedFilesystem>, String> {
    let resolved = if path.exists() {
        std::fs::canonicalize(path)
            .map_err(|error| format!("解析路径 {} 失败: {error}", path.display()))?
    } else {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let parent = std::fs::canonicalize(parent)
            .map_err(|error| format!("解析父目录 {} 失败: {error}", parent.display()))?;
        path.file_name()
            .map_or(parent.clone(), |name| parent.join(name))
    };
    Ok(mounted_filesystems()?
        .into_iter()
        .filter(|mount| resolved.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.components().count()))
}

fn parse_mountinfo(text: &str) -> Result<Vec<MountedFilesystem>, String> {
    let mut mounts = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        let (before, after) = line
            .split_once(" - ")
            .ok_or_else(|| format!("mountinfo 第 {} 行缺少分隔符", line_number + 1))?;
        let fields: Vec<&str> = before.split_whitespace().collect();
        let trailing: Vec<&str> = after.split_whitespace().collect();
        if fields.len() < 5 || trailing.len() < 2 {
            return Err(format!("mountinfo 第 {} 行字段不足", line_number + 1));
        }
        let filesystem_type = trailing[0].to_string();
        let source = unescape_mountinfo(trailing[1]);
        mounts.push(MountedFilesystem {
            mount_point: PathBuf::from(unescape_mountinfo(fields[4])),
            network: is_network_filesystem(&filesystem_type),
            filesystem_type,
            source,
        });
    }
    mounts.sort_by(|left, right| {
        right
            .network
            .cmp(&left.network)
            .then_with(|| left.mount_point.cmp(&right.mount_point))
    });
    mounts.dedup_by(|left, right| left.mount_point == right.mount_point);
    Ok(mounts)
}

fn unescape_mountinfo(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn is_network_filesystem(filesystem_type: &str) -> bool {
    matches!(
        filesystem_type,
        "nfs" | "nfs4" | "cifs" | "smb3" | "9p" | "ceph" | "glusterfs" | "sshfs"
    ) || filesystem_type.starts_with("fuse.sshfs")
}

pub fn browse_directory(path: &Path) -> Result<Vec<BrowserEntry>, String> {
    let mut entries = std::fs::read_dir(path)
        .map_err(|error| format!("读取目录 {} 失败: {error}", path.display()))?
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| format!("读取 {} 元数据失败: {error}", entry.path().display()))?;
            let kind = if metadata.file_type().is_symlink() {
                BrowserEntryKind::Symlink
            } else if metadata.is_dir() {
                BrowserEntryKind::Directory
            } else if metadata.is_file() {
                BrowserEntryKind::File
            } else {
                BrowserEntryKind::Other
            };
            Ok(BrowserEntry {
                path: entry.path(),
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
                size: metadata.is_file().then_some(metadata.len()),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    entries.sort_by(|left, right| {
        matches!(right.kind, BrowserEntryKind::Directory)
            .cmp(&matches!(left.kind, BrowserEntryKind::Directory))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaLifecycle {
    NoMediaDetected,
    PresentUnthreaded,
    LoadedThreaded,
    Transitioning,
    Unknown,
}

fn media_lifecycle(media: Option<&device::MediaInfo>) -> MediaLifecycle {
    match media {
        Some(media) => match media.presence {
            device::MediaPresence::Loaded => MediaLifecycle::LoadedThreaded,
            device::MediaPresence::NotLoaded if media.mam.is_some() => {
                MediaLifecycle::PresentUnthreaded
            }
            device::MediaPresence::NotLoaded => MediaLifecycle::NoMediaDetected,
            device::MediaPresence::NotReady => MediaLifecycle::Transitioning,
            device::MediaPresence::Unknown => MediaLifecycle::Unknown,
        },
        None => MediaLifecycle::Unknown,
    }
}

#[derive(Debug, Clone)]
pub struct DeviceSnapshot {
    pub drive: TapeDrive,
    pub lifecycle: MediaLifecycle,
    pub media: Option<device::MediaInfo>,
    pub volume: Option<VolumeInfo>,
    pub diagnosis: Option<VolumeDiagnosis>,
    pub health: Option<DriveHealth>,
    pub refreshed_at: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChannelTelemetryFrame {
    pub rates: Vec<device::channel_error::ChannelRate>,
    pub worst_now: Option<f64>,
    pub session_worst: Option<(usize, f64)>,
    pub last_success: Option<String>,
    pub stale: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
pub struct ChannelTelemetryTracker {
    previous: Option<Vec<device::channel_error::ChannelCounters>>,
    frame: ChannelTelemetryFrame,
}

impl ChannelTelemetryTracker {
    pub fn observe(
        &mut self,
        health: Option<&DriveHealth>,
        timestamp: &str,
    ) -> ChannelTelemetryFrame {
        let Some(counters) = health.and_then(|health| health.write_channels.as_ref()) else {
            self.frame.stale = !self.frame.rates.is_empty();
            self.frame.last_error = Some("write channel diagnostic unavailable".into());
            return self.frame.clone();
        };
        if let Some(previous) = self.previous.replace(counters.clone()) {
            self.frame.rates = device::channel_error::rates(&previous, counters);
            self.frame.worst_now = device::channel_error::worst_rate(&self.frame.rates);
            if let Some(rate) = self.frame.worst_now.filter(|rate| *rate < 0.0)
                && let Some(channel) = self.frame.rates.iter().find_map(|channel| {
                    channel
                        .log10_bit_error_rate
                        .filter(|value| value.total_cmp(&rate).is_eq())
                        .map(|_| channel.channel)
                })
                && self
                    .frame
                    .session_worst
                    .is_none_or(|(_, current)| rate > current)
            {
                self.frame.session_worst = Some((channel, rate));
            }
            self.frame.last_success = Some(timestamp.into());
        }
        self.frame.stale = false;
        self.frame.last_error = None;
        self.frame.clone()
    }

    pub fn mark_error(&mut self, error: impl Into<String>) -> ChannelTelemetryFrame {
        self.frame.stale = true;
        self.frame.last_error = Some(error.into());
        self.frame.clone()
    }
}

/// 为 Presentation 层建立一次一致的只读设备快照。
///
/// 调用方必须在统一设备 worker 中串行调用；TUI redraw 不得直接调用本函数。
pub fn pending_device_snapshot(drive: &TapeDrive) -> DeviceSnapshot {
    DeviceSnapshot {
        drive: drive.clone(),
        lifecycle: MediaLifecycle::Unknown,
        media: None,
        volume: None,
        diagnosis: None,
        health: None,
        refreshed_at: ltfs_time_now(),
        warnings: Vec::new(),
    }
}

/// 快速设备页快照；明确不定位磁带、不读取 LTFS label/index。
pub fn inspect_device_snapshot_basic(drive: &TapeDrive) -> DeviceSnapshot {
    let mut warnings = Vec::new();
    let media = match inspect_media(drive) {
        Ok(media) => Some(media),
        Err(error) => {
            warnings.push(format!("介质状态查询失败: {error}"));
            None
        }
    };
    let lifecycle = media_lifecycle(media.as_ref());
    let health = match read_drive_health(drive) {
        Ok(health) => Some(health),
        Err(error) => {
            warnings.push(format!("健康状态查询失败: {error}"));
            None
        }
    };

    DeviceSnapshot {
        drive: drive.clone(),
        lifecycle,
        media,
        volume: None,
        diagnosis: None,
        health,
        refreshed_at: ltfs_time_now(),
        warnings,
    }
}

/// 完整快照只应在用户明确请求读取 LTFS 后使用。
pub fn inspect_device_snapshot(drive: &TapeDrive) -> DeviceSnapshot {
    let mut snapshot = inspect_device_snapshot_basic(drive);
    if snapshot.lifecycle != MediaLifecycle::LoadedThreaded {
        return snapshot;
    }
    match inspect_volume(drive) {
        Ok(result) => {
            snapshot.warnings.extend(result.warnings.iter().cloned());
            snapshot.volume = Some(result);
        }
        Err(error) => snapshot.warnings.push(format!("LTFS 查询失败: {error}")),
    }
    if snapshot
        .volume
        .as_ref()
        .is_some_and(|volume| volume.recognized)
    {
        match diagnose_volume(drive) {
            Ok(result) => {
                snapshot
                    .warnings
                    .extend(result.partition_errors.iter().cloned());
                snapshot.diagnosis = Some(result);
            }
            Err(error) => snapshot.warnings.push(format!("一致性诊断失败: {error}")),
        }
    }
    snapshot.refreshed_at = ltfs_time_now();
    snapshot
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
    /// index 分区中最新 index 前的 filemark 块号（刷新 index 时的覆盖起点）。
    pub index_write_block: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCandidateDiagnostic {
    pub physical_partition: u8,
    pub actual_start_block: u64,
    pub byte_len: usize,
    pub index: Option<Index>,
    pub parse_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeConsistency {
    Healthy,
    MamUnavailable,
    NoUsableIndex,
    IndexCopyMissing,
    DivergentIndexes,
    ForeignIndex,
    InvalidIndexLocation,
    StaleVci,
    DivergentVci,
    DivergentLabels,
    UnindexedTail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeDiagnosis {
    pub label_uuid: Option<String>,
    pub candidates: Vec<IndexCandidateDiagnostic>,
    pub vci: Vec<(u8, VolumeCoherencyInformation)>,
    pub partition_errors: Vec<String>,
    pub consistency: VolumeConsistency,
    pub safe_for_normal_write: bool,
}

fn classify_volume_consistency(
    label_uuid: Option<&str>,
    candidates: &[IndexCandidateDiagnostic],
    vci: &[(u8, VolumeCoherencyInformation)],
) -> (VolumeConsistency, bool) {
    let valid: Vec<&IndexCandidateDiagnostic> = candidates
        .iter()
        .filter(|candidate| candidate.index.is_some())
        .collect();
    if valid.is_empty() {
        return (VolumeConsistency::NoUsableIndex, false);
    }
    if label_uuid.is_some_and(|uuid| {
        valid
            .iter()
            .any(|candidate| candidate.index.as_ref().unwrap().volume_uuid != uuid)
    }) {
        return (VolumeConsistency::ForeignIndex, false);
    }
    if valid.iter().any(|candidate| {
        let index = candidate.index.as_ref().unwrap();
        index.self_location.partition != candidate.physical_partition
            || index.self_location.startblock != candidate.actual_start_block
    }) {
        return (VolumeConsistency::InvalidIndexLocation, false);
    }
    let latest_by_partition: Vec<&IndexCandidateDiagnostic> = [0u8, 1u8]
        .iter()
        .filter_map(|partition| {
            valid
                .iter()
                .filter(|candidate| candidate.physical_partition == *partition)
                .max_by_key(|candidate| candidate.index.as_ref().unwrap().generation)
                .copied()
        })
        .collect();
    if latest_by_partition.len() < 2 {
        return (VolumeConsistency::IndexCopyMissing, false);
    }
    if latest_by_partition.iter().any(|latest| {
        candidates.iter().any(|candidate| {
            candidate.physical_partition == latest.physical_partition
                && candidate.actual_start_block > latest.actual_start_block
        })
    }) {
        return (VolumeConsistency::UnindexedTail, false);
    }
    let first = latest_by_partition[0].index.as_ref().unwrap();
    if latest_by_partition.iter().skip(1).any(|candidate| {
        let index = candidate.index.as_ref().unwrap();
        index.generation != first.generation || index.volume_uuid != first.volume_uuid
    }) {
        return (VolumeConsistency::DivergentIndexes, false);
    }
    if vci.is_empty() {
        return (VolumeConsistency::MamUnavailable, true);
    }
    if vci.len() != 2
        || vci.iter().any(|(_, copy)| {
            copy.generation != vci[0].1.generation
                || copy.volume_uuid != vci[0].1.volume_uuid
                || copy.vcr != vci[0].1.vcr
        })
    {
        return (VolumeConsistency::DivergentVci, false);
    }
    if vci.iter().any(|(_, copy)| {
        copy.generation != first.generation || copy.volume_uuid != first.volume_uuid
    }) {
        return (VolumeConsistency::StaleVci, false);
    }
    if vci.iter().any(|(partition, copy)| {
        latest_by_partition
            .iter()
            .find(|candidate| candidate.physical_partition == *partition)
            .is_none_or(|candidate| copy.block != candidate.actual_start_block)
    }) {
        return (VolumeConsistency::StaleVci, false);
    }
    (VolumeConsistency::Healthy, true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MamDiagnosticAttribute {
    pub id: u16,
    pub format: u8,
    pub value: Vec<u8>,
    pub vci: Option<VolumeCoherencyInformation>,
    pub parse_warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionMamDiagnostic {
    pub partition: u8,
    pub attributes: Vec<MamDiagnosticAttribute>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MamDiagnostic {
    pub partitions: Vec<PartitionMamDiagnostic>,
    pub warnings: Vec<String>,
}

/// 读取两个 LTFS 分区的 MAM。partition 1 不存在时保留警告，不掩盖 partition 0。
pub fn inspect_mam(drive: &TapeDrive) -> Result<MamDiagnostic, String> {
    let mut tape = TapeSession::open(&drive.sg_path).map_err(|e| e.to_string())?;
    let mut report = MamDiagnostic::default();
    for partition in [0u8, 1u8] {
        match tape.read_mam_attributes(partition) {
            Ok(records) => {
                let attributes = records
                    .into_iter()
                    .filter(|record| matches!(record.id, 0x0800..=0x080c | 0x0820 | 0x0009))
                    .map(|record| {
                        let (vci, parse_warning) = if record.id == mam::VOLUME_COHERENCY_INFORMATION
                        {
                            match VolumeCoherencyInformation::parse(&record.value) {
                                Ok(vci) => (Some(vci), None),
                                Err(error) => (None, Some(error)),
                            }
                        } else {
                            (None, None)
                        };
                        MamDiagnosticAttribute {
                            id: record.id,
                            format: record.format,
                            value: record.value,
                            vci,
                            parse_warning,
                        }
                    })
                    .collect();
                report.partitions.push(PartitionMamDiagnostic {
                    partition,
                    attributes,
                });
            }
            Err(error) => report
                .warnings
                .push(format!("partition {partition} MAM 读取失败: {error}")),
        }
    }
    Ok(report)
}

/// 创建新 LTFS 卷所需的用户参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// ANSI/MAM 使用的 6 位卷序列。
    pub barcode: String,
    /// LTFS 根目录显示名。
    pub volume_name: String,
    /// LTFS record 最大大小；当前正常双分区工作流默认 512 KiB。
    pub block_size: u64,
    pub compression: bool,
}

impl FormatOptions {
    pub fn new(barcode: impl Into<String>, volume_name: impl Into<String>) -> Self {
        Self {
            barcode: barcode.into(),
            volume_name: volume_name.into(),
            block_size: 512 * 1024,
            compression: true,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        AnsiLabel {
            barcode: self.barcode.clone(),
        }
        .to_bytes()
        .map_err(|e| e.to_string())?;
        if self.volume_name.is_empty() {
            return Err("LTFS Volume Name 不能为空".into());
        }
        if self.volume_name.chars().any(char::is_control) {
            return Err("LTFS Volume Name 不能包含控制字符".into());
        }
        if self.block_size == 0 || self.block_size > 1024 * 1024 {
            return Err("LTFS block size 必须在 1..=1048576 字节范围内".into());
        }
        Ok(())
    }
}

#[derive(Debug)]
struct InitialFormatImage {
    ansi: [u8; crate::ltfs::label::ANSI_LABEL_LEN],
    data_label_xml: String,
    index_label_xml: String,
    data_index_xml: String,
    index_index_xml: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatPhase {
    Preparing,
    Partitioning,
    WritingMam,
    WritingDataPartition,
    WritingIndexPartition,
    WritingCoherency,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatEvent {
    pub phase: FormatPhase,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatResult {
    pub barcode: String,
    pub volume_name: String,
    pub volume_uuid: String,
    pub generation: u64,
}

pub struct FormatSession<'a> {
    drive: &'a TapeDrive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    Short,
    Long,
    MinimumPartitionLong,
}

impl EraseMode {
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Long => "long",
            Self::MinimumPartitionLong => "minimum",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErasePhase {
    Preparing,
    CreatingMinimumPartition,
    Rethreading,
    Erasing,
    RestoringUnpartitioned,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EraseEvent {
    pub phase: ErasePhase,
    /// 0..=65535，来自 REQUEST SENSE progress indication。
    pub progress: Option<u16>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EraseResult {
    pub mode: EraseMode,
    pub elapsed_seconds: u64,
}

pub struct EraseSession<'a> {
    drive: &'a TapeDrive,
}

impl<'a> EraseSession<'a> {
    pub fn new(drive: &'a TapeDrive) -> Self {
        Self { drive }
    }

    pub fn run(
        &self,
        mode: EraseMode,
        observer: &mut dyn FnMut(&EraseEvent),
    ) -> Result<EraseResult, String> {
        use std::time::Instant;

        let started = Instant::now();
        emit_erase(observer, ErasePhase::Preparing, None, "打开并独占磁带设备");
        let mut tape = TapeSession::open(&self.drive.sg_path).map_err(|e| e.to_string())?;
        match mode {
            EraseMode::Short => {
                emit_erase(observer, ErasePhase::Erasing, None, "执行 short erase");
                tape.short_erase().map_err(|e| e.to_string())?;
            }
            EraseMode::Long => {
                tape.load().map_err(|e| e.to_string())?;
                emit_erase(
                    observer,
                    ErasePhase::RestoringUnpartitioned,
                    None,
                    "移除现有分区，准备全带 long erase",
                );
                tape.remove_partitions().map_err(|e| e.to_string())?;
                emit_erase(
                    observer,
                    ErasePhase::Rethreading,
                    None,
                    "重新穿带以应用未分区布局",
                );
                tape.rethread().map_err(|e| e.to_string())?;
                run_long_erase(&mut tape, observer)?;
            }
            EraseMode::MinimumPartitionLong => {
                run_minimum_partition_long_erase(&mut tape, observer)?;
            }
        }
        emit_erase(observer, ErasePhase::Completed, None, "擦除完成");
        Ok(EraseResult {
            mode,
            elapsed_seconds: started.elapsed().as_secs(),
        })
    }
}

fn run_long_erase(
    tape: &mut TapeSession,
    observer: &mut dyn FnMut(&EraseEvent),
) -> Result<(), String> {
    emit_erase(observer, ErasePhase::Erasing, None, "启动 long erase");
    tape.start_long_erase().map_err(|e| e.to_string())?;
    loop {
        match tape.long_erase_status().map_err(|e| e.to_string())? {
            LongEraseStatus::Complete => return Ok(()),
            LongEraseStatus::InProgress { progress } => {
                emit_erase(observer, ErasePhase::Erasing, progress, "long erase 进行中");
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        }
    }
}

fn run_minimum_partition_long_erase(
    tape: &mut TapeSession,
    observer: &mut dyn FnMut(&EraseEvent),
) -> Result<(), String> {
    emit_erase(
        observer,
        ErasePhase::CreatingMinimumPartition,
        None,
        "创建最小 P0 临时分区",
    );
    tape.create_minimum_erase_partition()
        .map_err(|e| e.to_string())?;

    emit_erase(
        observer,
        ErasePhase::Rethreading,
        None,
        "重新穿带以应用临时分区",
    );
    let erase_result = tape
        .rethread()
        .map_err(|e| e.to_string())
        .and_then(|_| run_long_erase(tape, observer));

    emit_erase(
        observer,
        ErasePhase::RestoringUnpartitioned,
        None,
        "移除临时分区并恢复未分区介质",
    );
    let restore_result = tape
        .rethread()
        .map_err(|e| e.to_string())
        .and_then(|_| tape.remove_partitions().map_err(|e| e.to_string()));

    match (erase_result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(erase), Ok(())) => Err(erase),
        (Ok(()), Err(restore)) => Err(format!(
            "long erase 已完成，但恢复未分区介质失败：{restore}"
        )),
        (Err(erase), Err(restore)) => Err(format!(
            "long erase 失败：{erase}；恢复未分区介质也失败：{restore}"
        )),
    }
}

fn emit_erase(
    observer: &mut dyn FnMut(&EraseEvent),
    phase: ErasePhase,
    progress: Option<u16>,
    message: &str,
) {
    observer(&EraseEvent {
        phase,
        progress,
        message: message.into(),
    });
}

impl<'a> FormatSession<'a> {
    pub fn new(drive: &'a TapeDrive) -> Self {
        Self { drive }
    }

    pub fn run(
        &self,
        options: &FormatOptions,
        observer: &mut dyn FnMut(&FormatEvent),
    ) -> Result<FormatResult, String> {
        options.validate()?;
        emit_format(observer, FormatPhase::Preparing, "生成初始 LTFS metadata");
        let format_time = ltfs_time_now();
        let volume_uuid = generate_uuid_v4()?;
        let image = build_initial_format_image(options, &volume_uuid, &format_time)?;
        let mut tape = TapeSession::open(&self.drive.sg_path).map_err(|e| e.to_string())?;

        emit_format(
            observer,
            FormatPhase::Partitioning,
            "创建 index/data 两个分区",
        );
        tape.ensure_write_anywhere().map_err(|e| e.to_string())?;
        tape.format_ltfs_partitions().map_err(|e| e.to_string())?;
        tape.set_variable_block().map_err(|e| e.to_string())?;

        emit_format(
            observer,
            FormatPhase::WritingMam,
            "写入 LTFS MAM attributes",
        );
        for warning in write_format_mam(&mut tape, options, &format_time, &volume_uuid)? {
            emit_format(observer, FormatPhase::WritingMam, &warning);
        }

        emit_format(
            observer,
            FormatPhase::WritingDataPartition,
            "写入 data 分区 label/index",
        );
        write_initial_partition(
            &mut tape,
            1,
            &image.ansi,
            &image.data_label_xml,
            &image.data_index_xml,
        )?;
        emit_format(
            observer,
            FormatPhase::WritingIndexPartition,
            "写入 index 分区 label/index",
        );
        write_initial_partition(
            &mut tape,
            0,
            &image.ansi,
            &image.index_label_xml,
            &image.index_index_xml,
        )?;

        emit_format(
            observer,
            FormatPhase::WritingCoherency,
            "写入两分区 Volume Coherency Information",
        );
        update_volume_coherency(&mut tape, &volume_uuid, 1, &[(0, 5), (1, 5)])?;
        emit_format(observer, FormatPhase::Completed, "LTFS format 完成");
        Ok(FormatResult {
            barcode: options.barcode.clone(),
            volume_name: options.volume_name.clone(),
            volume_uuid,
            generation: 1,
        })
    }
}

fn emit_format(observer: &mut dyn FnMut(&FormatEvent), phase: FormatPhase, message: &str) {
    observer(&FormatEvent {
        phase,
        message: message.into(),
    });
}

fn write_initial_partition(
    tape: &mut TapeSession,
    partition: u8,
    ansi: &[u8],
    label_xml: &str,
    index_xml: &str,
) -> Result<(), String> {
    tape.locate(partition, 0).map_err(|e| e.to_string())?;
    tape.write_record(ansi).map_err(|e| e.to_string())?;
    tape.write_filemark().map_err(|e| e.to_string())?;
    tape.write_record(label_xml.as_bytes())
        .map_err(|e| e.to_string())?;
    tape.write_filemark().map_err(|e| e.to_string())?;
    tape.write_filemark().map_err(|e| e.to_string())?;
    let pos = tape.read_position().map_err(|e| e.to_string())?;
    if pos.partition != partition as u32 || pos.block != 5 {
        return Err(format!(
            "初始 index 位置异常：期望 p{partition}b5，实际 p{}b{}",
            pos.partition, pos.block
        ));
    }
    tape.write_record(index_xml.as_bytes())
        .map_err(|e| e.to_string())?;
    tape.write_filemark().map_err(|e| e.to_string())?;
    Ok(())
}

fn write_format_mam(
    tape: &mut TapeSession,
    options: &FormatOptions,
    format_time: &str,
    volume_uuid: &str,
) -> Result<Vec<String>, String> {
    let written = format_time
        .chars()
        .filter(char::is_ascii_digit)
        .take(12)
        .collect::<String>();
    let attributes = mam::format_host_attributes(
        &options.volume_name,
        &options.barcode,
        volume_uuid,
        env!("CARGO_PKG_VERSION"),
        &written,
    );
    let mut warnings = Vec::new();
    for attribute in attributes {
        let format = match attribute.format {
            ValueFormat::Binary => MamAttributeFormat::Binary,
            ValueFormat::Ascii => MamAttributeFormat::Ascii,
            ValueFormat::Text => MamAttributeFormat::Text,
        };
        if let Err(error) = tape.write_mam_attribute(0, attribute.id, format, &attribute.value) {
            if attribute.required {
                return Err(error.to_string());
            }
            warnings.push(format!(
                "可选 MAM attribute 0x{:04X} 不受支持：{}",
                attribute.id, error
            ));
        }
    }
    Ok(warnings)
}

fn update_volume_coherency(
    tape: &mut TapeSession,
    volume_uuid: &str,
    generation: u64,
    partitions: &[(u8, u64)],
) -> Result<(), String> {
    // LTFS 2.4 §10.3：index 完成后先落带，再立即读 VCR，随后为所有完整
    // 分区写 VCI。WRITE ATTRIBUTE 不写逻辑对象，不会使刚读取的 VCR 失效。
    tape.flush().map_err(|e| e.to_string())?;
    let vcr = tape
        .read_mam_attribute(0, mam::VOLUME_CHANGE_REFERENCE)
        .map_err(|e| e.to_string())?;
    for &(partition, block) in partitions {
        let value =
            VolumeCoherencyInformation::new(&vcr, generation, block, volume_uuid)?.to_bytes()?;
        tape.write_mam_attribute(
            partition,
            mam::VOLUME_COHERENCY_INFORMATION,
            MamAttributeFormat::Binary,
            &value,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn update_write_volume_coherency(
    tape: &mut TapeSession,
    volume_uuid: &str,
    generation: u64,
    partitions: &[(u8, u64)],
    options: &WriteOptions,
) -> Result<(), String> {
    tape.flush().map_err(|e| e.to_string())?;
    check_write_stop(options, WriteFailpoint::AfterIndexes)?;
    let vcr = tape
        .read_mam_attribute(0, mam::VOLUME_CHANGE_REFERENCE)
        .map_err(|e| e.to_string())?;
    for (index, &(partition, block)) in partitions.iter().enumerate() {
        let value =
            VolumeCoherencyInformation::new(&vcr, generation, block, volume_uuid)?.to_bytes()?;
        tape.write_mam_attribute(
            partition,
            mam::VOLUME_COHERENCY_INFORMATION,
            MamAttributeFormat::Binary,
            &value,
        )
        .map_err(|e| e.to_string())?;
        if index == 0 {
            check_write_stop(options, WriteFailpoint::AfterFirstVci)?;
        }
    }
    Ok(())
}

fn generate_uuid_v4() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| format!("生成 volume UUID 失败: {e}"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

/// 生成格式化时要写入两个分区的全部 LTFS metadata。
///
/// 位置对应标准初始布局：VOL1@0, FM@1, label@2, FM@3, FM@4,
/// generation-1 index@5, FM@6。
fn build_initial_format_image(
    options: &FormatOptions,
    volume_uuid: &str,
    format_time: &str,
) -> Result<InitialFormatImage, String> {
    options.validate()?;
    let creator = format!("tapecpy {} - Linux - format", env!("CARGO_PKG_VERSION"));
    let ansi = AnsiLabel {
        barcode: options.barcode.clone(),
    }
    .to_bytes()
    .map_err(|e| e.to_string())?;
    let base_label = Label {
        version: "2.4.0".into(),
        creator: creator.clone(),
        format_time: format_time.into(),
        volume_uuid: volume_uuid.into(),
        blocksize: options.block_size,
        compression: options.compression,
        this_partition: 1,
        index_partition: 0,
        data_partition: 1,
    };
    let data_label_xml = base_label.to_xml();
    let mut index_label = base_label;
    index_label.this_partition = 0;
    let index_label_xml = index_label.to_xml();

    let times = FileTimes {
        creation_time: Some(format_time.into()),
        change_time: Some(format_time.into()),
        modify_time: Some(format_time.into()),
        access_time: Some(format_time.into()),
        backup_time: Some(format_time.into()),
    };
    let data_pos = TapePos {
        partition: 1,
        startblock: 5,
    };
    let mut initial_index = Index {
        version: "2.4.0".into(),
        creator,
        volume_uuid: volume_uuid.into(),
        generation: 1,
        update_time: format_time.into(),
        self_location: data_pos.clone(),
        previous_location: None,
        allow_policy_update: false,
        volume_lock_state: Some("unlocked".into()),
        highest_file_uid: Some(1),
        root: crate::ltfs::index::Directory {
            name: options.volume_name.clone(),
            fileuid: 1,
            readonly: false,
            times,
            extended_attributes: Vec::new(),
            entries: Vec::new(),
        },
    };
    let data_index_xml = initial_index.to_xml();
    initial_index.self_location = TapePos {
        partition: 0,
        startblock: 5,
    };
    initial_index.previous_location = Some(data_pos);
    let index_index_xml = initial_index.to_xml();

    Ok(InitialFormatImage {
        ansi,
        data_label_xml,
        index_label_xml,
        data_index_xml,
        index_index_xml,
    })
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
) -> Result<(Option<Index>, Option<u64>), device::Error> {
    // label 结束于块 3（VOL1, FM, XML label, FM；某些实现后跟一个多余 FM），
    // index 从块 4 开始。
    session.locate(partition, 4)?;

    // 不依赖驱动器的 EOD 报告（LTO 驱动器在块模式切换后可能返回错误位置）：
    // 直接前向读取直到 blank check（EOD）。同时跟踪最后一个有效 index 文件
    // 的起始块。LTFS 刷新 index 分区时从最新 index 前的 filemark 开始
    // 覆盖，而不是在分区 EOD 后追加。
    let mut records: Vec<u8> = Vec::new();
    let mut latest: Option<Index> = None;
    let mut last_valid_file_start: Option<u64> = None;
    let mut file_start = 4u64;
    let mut file_records = 0u64;
    loop {
        match session.read_record()? {
            ReadRecord::Data(buf) => {
                records.extend_from_slice(&buf);
                file_records += 1;
            }
            ReadRecord::Filemark => {
                if !records.is_empty()
                    && let Ok(text) = std::str::from_utf8(&records)
                    && let Ok(idx) = Index::parse_xml(text)
                {
                    last_valid_file_start = Some(file_start);
                    latest = Some(idx);
                }
                records.clear();
                file_start += file_records + 1;
                file_records = 0;
            }
            ReadRecord::Eod => break,
        }
    }
    let write_block = last_valid_file_start.and_then(|start| start.checked_sub(1));
    Ok((latest, write_block))
}

fn scan_index_candidates(
    session: &mut TapeSession,
    partition: u8,
) -> Result<Vec<IndexCandidateDiagnostic>, device::Error> {
    session.locate(partition, 4)?;
    let mut candidates = Vec::new();
    let mut records = Vec::new();
    let mut byte_len = 0usize;
    let mut looks_like_index = None;
    let mut file_start = 4u64;
    let mut file_records = 0u64;
    loop {
        match session.read_record()? {
            ReadRecord::Data(data) => {
                byte_len = byte_len.saturating_add(data.len());
                let is_index = *looks_like_index.get_or_insert_with(|| {
                    data.starts_with(b"<?xml") || data.starts_with(b"\xef\xbb\xbf<?xml")
                });
                // 普通 data file 可能占满整盘，绝不能为诊断把它全部缓存到内存。
                if is_index {
                    records.extend_from_slice(&data);
                }
                file_records += 1;
            }
            ReadRecord::Filemark => {
                if byte_len > 0 {
                    candidates.push(index_candidate_from_records(
                        partition,
                        file_start,
                        byte_len,
                        looks_like_index == Some(true),
                        &records,
                    ));
                }
                records.clear();
                byte_len = 0;
                looks_like_index = None;
                file_start += file_records + 1;
                file_records = 0;
            }
            ReadRecord::Eod => {
                if byte_len > 0 {
                    candidates.push(index_candidate_from_records(
                        partition,
                        file_start,
                        byte_len,
                        looks_like_index == Some(true),
                        &records,
                    ));
                }
                return Ok(candidates);
            }
        }
    }
}

fn index_candidate_from_records(
    partition: u8,
    start_block: u64,
    byte_len: usize,
    looks_like_index: bool,
    records: &[u8],
) -> IndexCandidateDiagnostic {
    let (index, parse_error) = if !looks_like_index {
        (
            None,
            Some("record group 不是 XML index（可能是普通 data）".into()),
        )
    } else {
        match std::str::from_utf8(records) {
            Ok(xml) => match Index::parse_xml(xml.trim_start_matches('\u{feff}')) {
                Ok(index) => (Some(index), None),
                Err(error) => (None, Some(error.to_string())),
            },
            Err(error) => (None, Some(format!("index 不是有效 UTF-8: {error}"))),
        }
    };
    IndexCandidateDiagnostic {
        physical_partition: partition,
        actual_start_block: start_block,
        byte_len,
        index,
        parse_error,
    }
}

fn read_index_candidate_at(
    session: &mut TapeSession,
    partition: u8,
    start_block: u64,
) -> Result<IndexCandidateDiagnostic, device::Error> {
    const MAX_INDEX_BYTES: usize = 512 * 1024 * 1024;
    session.locate(partition, start_block)?;
    let mut records = Vec::new();
    let mut byte_len = 0usize;
    let mut looks_like_index = None;
    loop {
        match session.read_record()? {
            ReadRecord::Data(data) => {
                byte_len = byte_len.saturating_add(data.len());
                let is_index = *looks_like_index.get_or_insert_with(|| {
                    data.starts_with(b"<?xml") || data.starts_with(b"\xef\xbb\xbf<?xml")
                });
                if !is_index {
                    return Ok(index_candidate_from_records(
                        partition,
                        start_block,
                        byte_len,
                        false,
                        &[],
                    ));
                }
                if records.len().saturating_add(data.len()) > MAX_INDEX_BYTES {
                    return Ok(IndexCandidateDiagnostic {
                        physical_partition: partition,
                        actual_start_block: start_block,
                        byte_len,
                        index: None,
                        parse_error: Some(format!(
                            "index candidate 超过诊断上限 {MAX_INDEX_BYTES} bytes"
                        )),
                    });
                }
                records.extend_from_slice(&data);
            }
            ReadRecord::Filemark | ReadRecord::Eod => {
                return Ok(index_candidate_from_records(
                    partition,
                    start_block,
                    byte_len,
                    looks_like_index == Some(true),
                    &records,
                ));
            }
        }
    }
}

/// 检查一台磁带机上的 LTFS 卷（Milestone 2）。
///
/// 流程：读取两个物理分区的 label → 确定 index 分区 → 扫描最新 index。
pub fn inspect_volume(drive: &TapeDrive) -> Result<VolumeInfo, device::Error> {
    let mut session = TapeSession::open(&drive.sg_path)?;
    inspect_volume_session(&mut session)
}

/// 只读扫描两个 partition 的 label、所有 index 文件和 VCI，用于不一致卷诊断。
/// 与普通 `inspect_volume` 不同，本函数不静默丢弃损坏的 index candidate。
pub fn diagnose_volume(drive: &TapeDrive) -> Result<VolumeDiagnosis, String> {
    diagnose_volume_with_options(drive, false)
}

pub fn diagnose_volume_full(drive: &TapeDrive) -> Result<VolumeDiagnosis, String> {
    diagnose_volume_with_options(drive, true)
}

fn diagnose_volume_with_options(
    drive: &TapeDrive,
    full_data_scan: bool,
) -> Result<VolumeDiagnosis, String> {
    let mut session = TapeSession::open(&drive.sg_path).map_err(|error| error.to_string())?;
    session
        .set_variable_block()
        .map_err(|error| error.to_string())?;
    let mut label_uuids = Vec::new();
    let mut candidates = Vec::new();
    let mut vci = Vec::new();
    let mut partition_errors = Vec::new();

    for partition in [0u8, 1u8] {
        match probe_partition_label(&mut session, partition) {
            Ok(Some((_, label))) => label_uuids.push((partition, label.volume_uuid)),
            Ok(None) => partition_errors.push(format!("partition {partition}: 无有效 LTFS label")),
            Err(error) => partition_errors.push(format!(
                "partition {partition}: label 读取失败（不是不存在）: {error}"
            )),
        }
        match session.read_mam_attribute(partition, mam::VOLUME_COHERENCY_INFORMATION) {
            Ok(raw) => match VolumeCoherencyInformation::parse(&raw) {
                Ok(copy) => vci.push((partition, copy)),
                Err(error) => {
                    partition_errors.push(format!("partition {partition}: VCI 解析失败: {error}"))
                }
            },
            Err(error) => {
                partition_errors.push(format!("partition {partition}: VCI 读取失败: {error}"))
            }
        }
    }

    match scan_index_candidates(&mut session, 0) {
        Ok(mut found) => candidates.append(&mut found),
        Err(error) => {
            partition_errors.push(format!("partition 0: index 扫描发生设备错误: {error}"))
        }
    }
    if full_data_scan {
        match scan_index_candidates(&mut session, 1) {
            Ok(mut found) => candidates.append(&mut found),
            Err(error) => {
                partition_errors.push(format!("partition 1: index 扫描发生设备错误: {error}"))
            }
        }
    } else {
        let mut targets: Vec<u64> = vci
            .iter()
            .filter(|(partition, _)| *partition == 1)
            .map(|(_, copy)| copy.block)
            .collect();
        targets.extend(candidates.iter().filter_map(|candidate| {
            candidate
                .index
                .as_ref()?
                .previous_location
                .as_ref()
                .filter(|location| location.partition == 1)
                .map(|location| location.startblock)
        }));
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            partition_errors.push(
                "partition 1: 没有 VCI/index chain 可信目标，默认有界诊断未执行全分区扫描".into(),
            );
        }
        for block in targets {
            match read_index_candidate_at(&mut session, 1, block) {
                Ok(candidate) => candidates.push(candidate),
                Err(error) => partition_errors.push(format!(
                    "partition 1 block {block}: 定点 index 读取失败: {error}"
                )),
            }
        }
    }

    let label_uuid = label_uuids.first().map(|(_, uuid)| uuid.clone());
    let divergent_labels = label_uuids
        .iter()
        .skip(1)
        .any(|(_, uuid)| Some(uuid) != label_uuid.as_ref());
    if divergent_labels {
        partition_errors.push(format!(
            "两个 partition 的 label UUID 不一致: {label_uuids:?}"
        ));
    }
    let (mut consistency, mut safe_for_normal_write) =
        classify_volume_consistency(label_uuid.as_deref(), &candidates, &vci);
    if label_uuids.len() != 2 {
        safe_for_normal_write = false;
        if consistency == VolumeConsistency::Healthy {
            consistency = VolumeConsistency::IndexCopyMissing;
        }
    } else if divergent_labels {
        consistency = VolumeConsistency::DivergentLabels;
        safe_for_normal_write = false;
    }
    Ok(VolumeDiagnosis {
        label_uuid,
        candidates,
        vci,
        partition_errors,
        consistency,
        safe_for_normal_write,
    })
}

/// 用已有会话检查 LTFS 卷（供需要继续使用同一会话的命令复用）。
fn inspect_volume_session(session: &mut TapeSession) -> Result<VolumeInfo, device::Error> {
    session.set_variable_block()?;

    let mut info = VolumeInfo::default();
    let mut labels: Vec<(u8, AnsiLabel, Label)> = Vec::new();

    // 先倒带到当前分区 BOT 并确认所在分区，优先探测当前分区，
    // 减少跨分区 LOCATE（LTO 驱动器上每次跨分区约 10 秒）。
    session.rewind()?;
    let start_partition = session.read_position()?.partition as u8;
    let order = [start_partition, 1 - start_partition];

    for &partition in &order {
        match probe_partition_label(session, partition) {
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
        match scan_latest_index(session, partition) {
            Ok((Some(idx), write_block)) => {
                if idx.self_location.partition == index_logical || info.index.is_none() {
                    info.index_write_block = write_block;
                    info.index = Some(idx);
                    break;
                }
            }
            Ok((None, _)) => {}
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

/// 读取 LTFS 卷中的文件内容（按 extent 定位到磁带数据分区，流式写入 `out`）。
///
/// 返回实际写入的字节数。设备错误以文本形式返回（保留底层细节）。
pub fn read_file(
    drive: &TapeDrive,
    path: &str,
    out: &mut dyn std::io::Write,
) -> Result<u64, String> {
    let mut ignore = |_: &ReadEvent| {};
    read_file_with_observer(drive, path, out, &CancellationToken::default(), &mut ignore)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadEvent {
    pub tape_path: String,
    pub bytes_read: u64,
    pub bytes_total: u64,
    pub partition: Option<u8>,
    pub logical_block: Option<u64>,
}

/// 可由 detached runner 观察和安全取消的单文件读取入口。
///
/// cancellation 在磁带 record 边界检查，不会通过终止线程打断 SG_IO。
pub fn read_file_with_observer(
    drive: &TapeDrive,
    path: &str,
    out: &mut dyn std::io::Write,
    cancellation: &CancellationToken,
    observer: &mut dyn FnMut(&ReadEvent),
) -> Result<u64, String> {
    let mut session = TapeSession::open(&drive.sg_path).map_err(|e| e.to_string())?;
    let volume = inspect_volume_session(&mut session).map_err(|e| e.to_string())?;
    if !volume.recognized {
        return Err(volume.reason.unwrap_or_else(|| "不是 LTFS 卷".into()));
    }
    let index = volume.index.ok_or_else(|| "没有可用的 index".to_string())?;
    let file = index
        .find_file(path)
        .ok_or_else(|| format!("文件不存在: {path}"))?;
    let bytes_total = file.length;

    let mut written = 0u64;
    for extent in &file.extents {
        session
            .locate(extent.partition, extent.start_block)
            .map_err(|e| e.to_string())?;
        let mut remaining = extent.byte_count;
        let mut logical_block = extent.start_block;
        while remaining > 0 {
            if cancellation.is_cancelled() {
                return Err("[cancelled]用户请求在磁带 record 边界停止读取".into());
            }
            match session.read_record().map_err(|e| e.to_string())? {
                ReadRecord::Data(buf) => {
                    let n = buf.len().min(remaining as usize);
                    out.write_all(&buf[..n])
                        .map_err(|e| format!("写入输出失败: {e}"))?;
                    written += n as u64;
                    remaining -= n as u64;
                    logical_block += 1;
                    observer(&ReadEvent {
                        tape_path: path.into(),
                        bytes_read: written,
                        bytes_total,
                        partition: Some(extent.partition),
                        logical_block: Some(logical_block),
                    });
                }
                _ => return Err(format!("读取 {path} 时磁带记录意外结束")),
            }
        }
    }
    Ok(written)
}

/// 写入结果摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct WriteResult {
    pub bytes: u64,
    pub files: usize,
    pub directories: usize,
    pub file_uid: u64,
    pub generation: u64,
    pub data_start_block: u64,
    pub hashes: Vec<FileHash>,
    pub verification: WriteVerification,
    pub health_before: DriveHealth,
    pub health_after: DriveHealth,
    pub health_delta: DriveHealthDelta,
    pub telemetry_history: Vec<ChannelTelemetrySample>,
    pub session_worst_channel_rate: Option<SessionWorstChannelRate>,
    pub telemetry_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHash {
    pub path: String,
    pub sha256: String,
}

/// 校验类型保持显式，避免与未来的 SCSI VERIFY 或驱动器介质校验混淆。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WriteVerification {
    #[default]
    None,
    /// 两个 index 和 MAM VCI 提交后，从磁带回读并比较 SHA-256。
    ReadBackSha256,
}

#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    pub verification: WriteVerification,
    /// TUI 最终确认时冻结的 source plan；runner 必须在首条 WRITE 前复核。
    pub expected_source: Option<WritePlanExpectation>,
    /// 仅用于可破坏集成测试；在已完成的语义步骤边界停止工作流。
    pub failpoint: Option<WriteFailpoint>,
    pub cancellation: Option<CancellationToken>,
    /// 仅供集成测试在确定的安全边界触发 cancellation token。
    pub cancelpoint: Option<WriteFailpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WritePlanExpectation {
    pub files: usize,
    pub directories: usize,
    pub payload_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl CancellationToken {
    pub fn request_cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteFailpoint {
    AfterFirstDataRecord,
    AfterDataIndex,
    AfterIndexes,
    AfterFirstVci,
    BeforeVerify,
}

impl std::str::FromStr for WriteFailpoint {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "after-first-data-record" => Ok(Self::AfterFirstDataRecord),
            "after-data-index" => Ok(Self::AfterDataIndex),
            "after-indexes" => Ok(Self::AfterIndexes),
            "after-first-vci" => Ok(Self::AfterFirstVci),
            "before-verify" => Ok(Self::BeforeVerify),
            _ => Err(format!("未知 write failpoint: {value}")),
        }
    }
}

fn check_write_stop(options: &WriteOptions, point: WriteFailpoint) -> Result<(), String> {
    if options.cancelpoint == Some(point)
        && let Some(token) = &options.cancellation
    {
        token.request_cancel();
    }
    if options
        .cancellation
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(format!("[cancelled]用户在安全边界 {point:?} 请求取消"));
    }
    if options.failpoint == Some(point) {
        let marker = if point == WriteFailpoint::AfterFirstVci {
            "[state:C4]"
        } else {
            ""
        };
        return Err(format!("{marker}测试 failpoint：在安全边界 {point:?} 停止"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WriteCommitState {
    #[default]
    NotStarted,
    DataIncomplete,
    DataIndexOnly,
    IndexesWritten,
    CoherencyPartial,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFailure {
    pub phase: WritePhase,
    pub commit_state: WriteCommitState,
    pub message: String,
    pub current_file: Option<String>,
    pub files_completed: usize,
    pub bytes_written: u64,
    pub partition: Option<u8>,
    pub logical_block: Option<u64>,
    pub safe_to_retry: bool,
    pub requires_diagnosis: bool,
    pub cancelled: bool,
}

impl std::fmt::Display for WriteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [phase={:?}, commit={:?}, bytes={}",
            self.message, self.phase, self.commit_state, self.bytes_written
        )?;
        if let (Some(partition), Some(block)) = (self.partition, self.logical_block) {
            write!(f, ", position=p{partition}b{block}")?;
        }
        f.write_str("]")
    }
}

impl std::error::Error for WriteFailure {}

pub const CHANNEL_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
pub const CHANNEL_HISTORY_CAPACITY: usize = 120;
pub const CHANNEL_DEFAULT_VISIBLE_SAMPLES: usize = 60;
pub const PERFORMANCE_HISTORY_CAPACITY: usize = 600;
pub const PERFORMANCE_DEFAULT_VISIBLE_SAMPLES: usize = 300;

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelTelemetrySample {
    pub elapsed_millis: u64,
    pub timestamp: String,
    pub partition: u8,
    pub logical_block: u64,
    /// 与上一个成功遥测点之间的应用层有效载荷吞吐（bytes/s）。
    pub throughput_bytes_per_second: f64,
    pub channel_rates: Vec<device::channel_error::ChannelRate>,
    pub worst_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WritePerformanceSample {
    pub timestamp: String,
    pub source_bytes_per_second: f64,
    pub tape_bytes_per_second: f64,
    pub buffer_used_bytes: u64,
    pub buffer_capacity_bytes: u64,
    pub reader_waiting: bool,
    pub writer_waiting: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionWorstChannelRate {
    pub rate: f64,
    pub channel: usize,
    pub elapsed_millis: u64,
    pub timestamp: String,
    pub partition: u8,
    pub logical_block: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePhase {
    Preparing,
    WritingData,
    FinalizingDataIndex,
    SyncingIndexPartition,
    UpdatingCoherency,
    Verifying,
    Failed,
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteEvent {
    pub phase: WritePhase,
    pub current_file: Option<String>,
    pub files_completed: usize,
    pub files_total: usize,
    pub bytes_written: u64,
    pub bytes_total: u64,
    pub partition: Option<u8>,
    pub logical_block: Option<u64>,
    pub telemetry: Option<ChannelTelemetrySample>,
    pub performance: Option<WritePerformanceSample>,
    pub failure: Option<WriteFailure>,
}

/// 一次完整 LTFS 写入事务的应用层入口。
///
/// 会话统一持有写入工作流的设备入口，并通过 observer 暴露可观察状态；
/// Presentation 层不需要了解分区切换、filemark 或 index 刷新细节。
pub struct WriteSession<'a> {
    drive: &'a TapeDrive,
}

impl<'a> WriteSession<'a> {
    pub fn new(drive: &'a TapeDrive) -> Self {
        Self { drive }
    }

    pub fn run(
        &self,
        local_path: &Path,
        tape_path: &str,
        observer: &mut dyn FnMut(&WriteEvent),
    ) -> Result<WriteResult, String> {
        self.run_with_options(local_path, tape_path, WriteOptions::default(), observer)
    }

    pub fn run_with_options(
        &self,
        local_path: &Path,
        tape_path: &str,
        options: WriteOptions,
        observer: &mut dyn FnMut(&WriteEvent),
    ) -> Result<WriteResult, String> {
        write_with_observer(
            self.drive,
            WriteSourceRequest::Local(local_path),
            tape_path,
            options,
            observer,
        )
    }

    pub fn run_detailed_with_options(
        &self,
        local_path: &Path,
        tape_path: &str,
        options: WriteOptions,
        observer: &mut dyn FnMut(&WriteEvent),
    ) -> Result<WriteResult, WriteFailure> {
        write_with_observer_detailed(
            self.drive,
            WriteSourceRequest::Local(local_path),
            tape_path,
            options,
            observer,
        )
    }

    pub fn run_roots_detailed_with_options(
        &self,
        local_roots: &[std::path::PathBuf],
        destination_directory: &str,
        options: WriteOptions,
        observer: &mut dyn FnMut(&WriteEvent),
    ) -> Result<WriteResult, WriteFailure> {
        write_with_observer_detailed(
            self.drive,
            WriteSourceRequest::LocalRoots(local_roots),
            destination_directory,
            options,
            observer,
        )
    }

    /// 写入一个不落盘、长度固定且可由 seed 重现的伪随机测试文件。
    pub fn run_pseudorandom(
        &self,
        size: u64,
        seed: u64,
        tape_path: &str,
        options: WriteOptions,
        observer: &mut dyn FnMut(&WriteEvent),
    ) -> Result<WriteResult, String> {
        write_with_observer(
            self.drive,
            WriteSourceRequest::Pseudorandom { size, seed },
            tape_path,
            options,
            observer,
        )
    }

    pub fn run_pseudorandom_detailed(
        &self,
        size: u64,
        seed: u64,
        tape_path: &str,
        options: WriteOptions,
        observer: &mut dyn FnMut(&WriteEvent),
    ) -> Result<WriteResult, WriteFailure> {
        write_with_observer_detailed(
            self.drive,
            WriteSourceRequest::Pseudorandom { size, seed },
            tape_path,
            options,
            observer,
        )
    }
}

struct WriteProgress<'a> {
    observer: &'a mut dyn FnMut(&WriteEvent),
    files_total: usize,
    bytes_total: u64,
    files_completed: usize,
    bytes_written: u64,
    partition: Option<u8>,
    logical_block: Option<u64>,
}

impl WriteProgress<'_> {
    fn emit(&mut self, phase: WritePhase, current_file: Option<&str>) {
        self.emit_with_telemetry(phase, current_file, None);
    }

    fn emit_with_telemetry(
        &mut self,
        phase: WritePhase,
        current_file: Option<&str>,
        telemetry: Option<ChannelTelemetrySample>,
    ) {
        (self.observer)(&WriteEvent {
            phase,
            current_file: current_file.map(str::to_owned),
            files_completed: self.files_completed,
            files_total: self.files_total,
            bytes_written: self.bytes_written,
            bytes_total: self.bytes_total,
            partition: self.partition,
            logical_block: self.logical_block,
            telemetry,
            performance: None,
            failure: None,
        });
    }

    fn emit_with_performance(
        &mut self,
        phase: WritePhase,
        current_file: Option<&str>,
        performance: WritePerformanceSample,
    ) {
        (self.observer)(&WriteEvent {
            phase,
            current_file: current_file.map(str::to_owned),
            files_completed: self.files_completed,
            files_total: self.files_total,
            bytes_written: self.bytes_written,
            bytes_total: self.bytes_total,
            partition: self.partition,
            logical_block: self.logical_block,
            telemetry: None,
            performance: Some(performance),
            failure: None,
        });
    }
}

struct WritePerformanceState {
    last_sample: std::time::Instant,
    last_source_bytes: u64,
    last_tape_bytes: u64,
}

impl WritePerformanceState {
    fn new() -> Self {
        Self {
            last_sample: std::time::Instant::now(),
            last_source_bytes: 0,
            last_tape_bytes: 0,
        }
    }

    fn sample_if_due(
        &mut self,
        pipeline: SourcePipelineSnapshot,
        tape_bytes: u64,
    ) -> Option<WritePerformanceSample> {
        self.sample(pipeline, tape_bytes, false)
    }

    fn sample(
        &mut self,
        pipeline: SourcePipelineSnapshot,
        tape_bytes: u64,
        force: bool,
    ) -> Option<WritePerformanceSample> {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_sample);
        if !force && elapsed < std::time::Duration::from_secs(1) {
            return None;
        }
        let sample = WritePerformanceSample {
            timestamp: ltfs_time_now(),
            source_bytes_per_second: payload_throughput(
                pipeline.bytes_read.saturating_sub(self.last_source_bytes),
                elapsed,
            ),
            tape_bytes_per_second: payload_throughput(
                tape_bytes.saturating_sub(self.last_tape_bytes),
                elapsed,
            ),
            buffer_used_bytes: pipeline.queued_bytes,
            buffer_capacity_bytes: pipeline.capacity_bytes,
            reader_waiting: pipeline.reader_waiting,
            writer_waiting: pipeline.writer_waiting,
        };
        self.last_sample = now;
        self.last_source_bytes = pipeline.bytes_read;
        self.last_tape_bytes = tape_bytes;
        Some(sample)
    }
}

struct ChannelTelemetryState {
    started: std::time::Instant,
    last_sample: std::time::Instant,
    last_throughput_sample: std::time::Instant,
    last_throughput_bytes: u64,
    previous: Option<Vec<device::channel_error::ChannelCounters>>,
    history: VecDeque<ChannelTelemetrySample>,
    session_worst: Option<SessionWorstChannelRate>,
    warnings: Vec<String>,
}

impl ChannelTelemetryState {
    fn new(previous: Option<Vec<device::channel_error::ChannelCounters>>) -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            last_sample: now,
            last_throughput_sample: now,
            last_throughput_bytes: 0,
            previous,
            history: VecDeque::with_capacity(CHANNEL_HISTORY_CAPACITY),
            session_worst: None,
            warnings: Vec::new(),
        }
    }

    fn sample_if_due(
        &mut self,
        session: &mut TapeSession,
        partition: u8,
        logical_block: u64,
        bytes_written: u64,
    ) -> Option<ChannelTelemetrySample> {
        if self.last_sample.elapsed() < CHANNEL_SAMPLE_INTERVAL {
            return None;
        }
        self.last_sample = std::time::Instant::now();
        let current = match read_channel_counters(session, device::channel_error::PageKind::Write) {
            Ok(current) => current,
            Err(error) => {
                self.warnings.push(error);
                return None;
            }
        };
        let previous = self.previous.replace(current.clone())?;
        let channel_rates = device::channel_error::rates(&previous, &current);
        let worst_rate = device::channel_error::worst_rate(&channel_rates);
        let throughput_now = std::time::Instant::now();
        let throughput_elapsed = throughput_now.duration_since(self.last_throughput_sample);
        let throughput_bytes = bytes_written.saturating_sub(self.last_throughput_bytes);
        let throughput_bytes_per_second = payload_throughput(throughput_bytes, throughput_elapsed);
        self.last_throughput_sample = throughput_now;
        self.last_throughput_bytes = bytes_written;
        let sample = ChannelTelemetrySample {
            elapsed_millis: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            timestamp: ltfs_time_now(),
            partition,
            logical_block,
            throughput_bytes_per_second,
            channel_rates,
            worst_rate,
        };
        self.record_sample(sample.clone());
        Some(sample)
    }

    fn begin_data(&mut self) {
        let now = std::time::Instant::now();
        self.started = now;
        self.last_sample = now;
        self.last_throughput_sample = now;
        self.last_throughput_bytes = 0;
    }

    fn record_sample(&mut self, sample: ChannelTelemetrySample) {
        self.update_session_worst(&sample);
        if self.history.len() == CHANNEL_HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(sample);
    }

    fn update_session_worst(&mut self, sample: &ChannelTelemetrySample) {
        let Some(rate) = sample.worst_rate.filter(|rate| *rate < 0.0) else {
            return;
        };
        let Some(channel) = sample.channel_rates.iter().find_map(|channel| {
            channel
                .log10_bit_error_rate
                .filter(|value| value.total_cmp(&rate).is_eq())
                .map(|_| channel.channel)
        }) else {
            return;
        };
        if self
            .session_worst
            .as_ref()
            .is_none_or(|current| rate > current.rate)
        {
            self.session_worst = Some(SessionWorstChannelRate {
                rate,
                channel,
                elapsed_millis: sample.elapsed_millis,
                timestamp: sample.timestamp.clone(),
                partition: sample.partition,
                logical_block: sample.logical_block,
            });
        }
    }
}

fn payload_throughput(bytes: u64, elapsed: std::time::Duration) -> f64 {
    if elapsed.is_zero() {
        0.0
    } else {
        bytes as f64 / elapsed.as_secs_f64()
    }
}

fn read_channel_counters(
    session: &mut TapeSession,
    kind: device::channel_error::PageKind,
) -> Result<Vec<device::channel_error::ChannelCounters>, String> {
    let raw = session
        .read_diagnostic_page(kind.page_code())
        .map_err(|error| format!("读取 channel page 0x{:02x} 失败: {error}", kind.page_code()))?;
    device::channel_error::parse_page(&raw, kind)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFileSnapshot {
    pub path: std::path::PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePlan {
    pub roots: Vec<std::path::PathBuf>,
    pub files: Vec<SourceFileSnapshot>,
    pub directories_total: usize,
    pub payload_bytes: u64,
    pub scanned_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceScanProgress {
    pub current_path: std::path::PathBuf,
    pub files_seen: usize,
    pub directories_seen: usize,
    pub payload_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacitySource {
    MamRemainingCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityStatus {
    Normal,
    WarningAboveNinetyPercent,
    BlockedInsufficient,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapacityAssessment {
    pub payload_bytes: u64,
    pub available_bytes: Option<u64>,
    pub planned_fraction: Option<f64>,
    pub status: CapacityStatus,
    pub source: Option<CapacitySource>,
    pub sampled_at: String,
}

pub fn assess_write_capacity(
    payload_bytes: u64,
    remaining_capacity_mib: Option<u64>,
    sampled_at: impl Into<String>,
) -> CapacityAssessment {
    let available_bytes = remaining_capacity_mib.and_then(|mib| mib.checked_mul(1024 * 1024));
    let status = match available_bytes {
        None => CapacityStatus::Unknown,
        Some(available) if payload_bytes > available => CapacityStatus::BlockedInsufficient,
        Some(available) if (payload_bytes as u128) * 10 > (available as u128) * 9 => {
            CapacityStatus::WarningAboveNinetyPercent
        }
        Some(_) => CapacityStatus::Normal,
    };
    CapacityAssessment {
        payload_bytes,
        available_bytes,
        planned_fraction: available_bytes.and_then(|available| {
            (available > 0).then_some(payload_bytes as f64 / available as f64)
        }),
        status,
        source: available_bytes.map(|_| CapacitySource::MamRemainingCapacity),
        sampled_at: sampled_at.into(),
    }
}

pub fn scan_source_roots(roots: &[std::path::PathBuf]) -> Result<SourcePlan, String> {
    let mut ignore = |_: &SourceScanProgress| {};
    scan_source_roots_with_observer(roots, &CancellationToken::default(), &mut ignore)
}

pub fn scan_source_roots_with_observer(
    roots: &[std::path::PathBuf],
    cancellation: &CancellationToken,
    observer: &mut dyn FnMut(&SourceScanProgress),
) -> Result<SourcePlan, String> {
    if roots.is_empty() {
        return Err("至少选择一个 source root".into());
    }
    let mut canonical_roots = Vec::with_capacity(roots.len());
    for root in roots {
        let root_metadata = std::fs::symlink_metadata(root)
            .map_err(|error| format!("读取 source {} 元数据失败: {error}", root.display()))?;
        if root_metadata.file_type().is_symlink() {
            return Err(format!("第一阶段暂不写入符号链接: {}", root.display()));
        }
        let canonical = std::fs::canonicalize(root)
            .map_err(|error| format!("解析 source {} 失败: {error}", root.display()))?;
        if canonical_roots.iter().any(|existing: &std::path::PathBuf| {
            canonical.starts_with(existing) || existing.starts_with(&canonical)
        }) {
            return Err(format!("source roots 重复或互相包含: {}", root.display()));
        }
        canonical_roots.push(canonical);
    }

    let mut plan = SourcePlan {
        roots: canonical_roots.clone(),
        files: Vec::new(),
        directories_total: 0,
        payload_bytes: 0,
        scanned_at: ltfs_time_now(),
    };
    for root in canonical_roots {
        scan_source_entry(&root, cancellation, observer, &mut plan)?;
    }
    Ok(plan)
}

pub fn plan_write_destination(
    index: &Index,
    source_root: &Path,
    destination_directory: &str,
) -> Result<String, String> {
    let name = source_root_name(source_root)?;
    let directory = index
        .find_directory(destination_directory)
        .ok_or_else(|| format!("LTFS destination 不存在: {destination_directory}"))?;
    if directory.readonly {
        return Err(format!(
            "LTFS destination 为 readonly: {destination_directory}"
        ));
    }
    if directory.entries.iter().any(|entry| entry.name() == name) {
        return Err(format!(
            "LTFS target 已存在: {destination_directory}/{name}"
        ));
    }
    Ok(if destination_directory == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", destination_directory.trim_end_matches('/'))
    })
}

pub fn validate_write_destinations(
    index: &Index,
    source_roots: &[std::path::PathBuf],
    destination_directory: &str,
) -> Result<Vec<String>, String> {
    let mut targets = Vec::with_capacity(source_roots.len());
    for root in source_roots {
        let target = plan_write_destination(index, root, destination_directory)?;
        if targets.iter().any(|existing| existing == &target) {
            return Err(format!(
                "多个 source root 映射到同一个 LTFS target: {target}"
            ));
        }
        targets.push(target);
    }
    Ok(targets)
}

fn source_root_name(source_root: &Path) -> Result<&str, String> {
    let name = source_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| {
            format!(
                "source root 没有可用的 LTFS 名称: {}",
                source_root.display()
            )
        })?;
    if name.contains('/') {
        return Err(format!("source 名称包含路径分隔符: {name}"));
    }
    Ok(name)
}

fn scan_source_entry(
    path: &Path,
    cancellation: &CancellationToken,
    observer: &mut dyn FnMut(&SourceScanProgress),
    plan: &mut SourcePlan,
) -> Result<(), String> {
    if cancellation.is_cancelled() {
        return Err("[cancelled]source scan 已取消".into());
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("读取 {} 元数据失败: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("第一阶段暂不写入符号链接: {}", path.display()));
    }
    if metadata.is_file() {
        std::fs::File::open(path)
            .map_err(|error| format!("打开 {} 失败: {error}", path.display()))?;
        plan.payload_bytes = plan
            .payload_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| "source payload bytes 溢出".to_string())?;
        plan.files.push(SourceFileSnapshot {
            path: path.to_path_buf(),
            size: metadata.len(),
        });
        observer(&SourceScanProgress {
            current_path: path.to_path_buf(),
            files_seen: plan.files.len(),
            directories_seen: plan.directories_total,
            payload_bytes: plan.payload_bytes,
        });
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!("source 不是普通文件或目录: {}", path.display()));
    }
    plan.directories_total += 1;
    observer(&SourceScanProgress {
        current_path: path.to_path_buf(),
        files_seen: plan.files.len(),
        directories_seen: plan.directories_total,
        payload_bytes: plan.payload_bytes,
    });
    let mut children = std::fs::read_dir(path)
        .map_err(|error| format!("读取目录 {} 失败: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("读取目录 {} 失败: {error}", path.display()))?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        if child.file_name().into_string().is_err() {
            return Err(format!("文件名不是 UTF-8: {}", child.path().display()));
        }
        scan_source_entry(&child.path(), cancellation, observer, plan)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PlannedFile {
    source: PlannedSource,
    target: String,
    size: u64,
}

#[derive(Debug, Clone)]
enum PlannedSource {
    Local(std::path::PathBuf),
    Pseudorandom { seed: u64 },
}

const DEFAULT_WRITE_BUFFER_BYTES: usize = 512 * 1024 * 1024;

enum SourcePipelineMessage {
    FileStart {
        target: String,
        description: String,
        expected_size: u64,
    },
    Data(Vec<u8>),
    FileEnd {
        actual_size: u64,
    },
    Error(String),
}

#[derive(Default)]
struct SourcePipelineMetrics {
    bytes_read: std::sync::atomic::AtomicU64,
    queued_bytes: std::sync::atomic::AtomicU64,
    reader_waiting: std::sync::atomic::AtomicBool,
    writer_waiting: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourcePipelineSnapshot {
    bytes_read: u64,
    queued_bytes: u64,
    capacity_bytes: u64,
    reader_waiting: bool,
    writer_waiting: bool,
}

struct SourcePipeline {
    messages: std::sync::mpsc::Receiver<SourcePipelineMessage>,
    recycle: std::sync::mpsc::SyncSender<Vec<u8>>,
    metrics: std::sync::Arc<SourcePipelineMetrics>,
    capacity_bytes: u64,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl SourcePipeline {
    fn spawn(
        files: Vec<PlannedFile>,
        block_size: usize,
        capacity_bytes: usize,
        cancellation: CancellationToken,
    ) -> Result<Self, String> {
        if block_size == 0 {
            return Err("write pipeline block size 不能为 0".into());
        }
        let block_capacity = capacity_bytes.div_ceil(block_size).max(1);
        let actual_capacity = block_capacity
            .checked_mul(block_size)
            .ok_or_else(|| "write pipeline capacity 溢出".to_string())?;
        let (message_tx, messages) = std::sync::mpsc::sync_channel(block_capacity + 2);
        let (recycle, recycle_rx) = std::sync::mpsc::sync_channel(block_capacity);
        let metrics = std::sync::Arc::new(SourcePipelineMetrics::default());
        let worker_metrics = metrics.clone();
        let worker = std::thread::Builder::new()
            .name("tapecpy-source-reader".into())
            .spawn(move || {
                let result = produce_source_blocks(
                    files,
                    block_size,
                    block_capacity,
                    cancellation,
                    &message_tx,
                    &recycle_rx,
                    &worker_metrics,
                );
                if let Err(error) = result {
                    let _ = message_tx.send(SourcePipelineMessage::Error(error));
                }
            })
            .map_err(|error| format!("启动 source reader 失败: {error}"))?;
        Ok(Self {
            messages,
            recycle,
            metrics,
            capacity_bytes: actual_capacity as u64,
            worker: Some(worker),
        })
    }

    #[cfg(test)]
    fn recv(&self) -> Result<SourcePipelineMessage, String> {
        self.metrics
            .writer_waiting
            .store(true, std::sync::atomic::Ordering::Release);
        let received = self.messages.recv();
        self.metrics
            .writer_waiting
            .store(false, std::sync::atomic::Ordering::Release);
        let message = received.map_err(|_| "source pipeline 在文件结束前关闭".to_string())?;
        if let SourcePipelineMessage::Data(data) = &message {
            self.metrics
                .queued_bytes
                .fetch_sub(data.len() as u64, std::sync::atomic::Ordering::AcqRel);
        }
        Ok(message)
    }

    fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Option<SourcePipelineMessage>, String> {
        self.metrics
            .writer_waiting
            .store(true, std::sync::atomic::Ordering::Release);
        let received = self.messages.recv_timeout(timeout);
        let message = match received {
            Ok(message) => {
                self.metrics
                    .writer_waiting
                    .store(false, std::sync::atomic::Ordering::Release);
                message
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(None),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.metrics
                    .writer_waiting
                    .store(false, std::sync::atomic::Ordering::Release);
                return Err("source pipeline 在文件结束前关闭".into());
            }
        };
        if let SourcePipelineMessage::Data(data) = &message {
            self.metrics
                .queued_bytes
                .fetch_sub(data.len() as u64, std::sync::atomic::Ordering::AcqRel);
        }
        Ok(Some(message))
    }

    fn recycle(&self, mut data: Vec<u8>) {
        data.clear();
        let _ = self.recycle.send(data);
    }

    fn snapshot(&self) -> SourcePipelineSnapshot {
        SourcePipelineSnapshot {
            bytes_read: self
                .metrics
                .bytes_read
                .load(std::sync::atomic::Ordering::Acquire),
            queued_bytes: self
                .metrics
                .queued_bytes
                .load(std::sync::atomic::Ordering::Acquire),
            capacity_bytes: self.capacity_bytes,
            reader_waiting: self
                .metrics
                .reader_waiting
                .load(std::sync::atomic::Ordering::Acquire),
            writer_waiting: self
                .metrics
                .writer_waiting
                .load(std::sync::atomic::Ordering::Acquire),
        }
    }

    fn join(mut self) -> Result<(), String> {
        self.worker
            .take()
            .expect("source pipeline worker exists")
            .join()
            .map_err(|_| "source reader thread panic".to_string())
    }
}

fn produce_source_blocks(
    files: Vec<PlannedFile>,
    block_size: usize,
    block_capacity: usize,
    cancellation: CancellationToken,
    messages: &std::sync::mpsc::SyncSender<SourcePipelineMessage>,
    recycle: &std::sync::mpsc::Receiver<Vec<u8>>,
    metrics: &SourcePipelineMetrics,
) -> Result<(), String> {
    let mut allocated = 0usize;
    for planned in files {
        if cancellation.is_cancelled() {
            return Err("[cancelled]source reader 在打开下一个文件前停止".into());
        }
        let description = planned.source.description();
        messages
            .send(SourcePipelineMessage::FileStart {
                target: planned.target,
                description: description.clone(),
                expected_size: planned.size,
            })
            .map_err(|_| "tape writer 已关闭 source pipeline".to_string())?;
        let mut source = planned.source.open(planned.size)?;
        let mut actual_size = 0u64;
        loop {
            if cancellation.is_cancelled() {
                return Err("[cancelled]source reader 在数据块边界停止".into());
            }
            let mut buffer = match recycle.try_recv() {
                Ok(buffer) => buffer,
                Err(std::sync::mpsc::TryRecvError::Empty) if allocated < block_capacity => {
                    allocated += 1;
                    Vec::with_capacity(block_size)
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    metrics
                        .reader_waiting
                        .store(true, std::sync::atomic::Ordering::Release);
                    let result = recycle.recv();
                    metrics
                        .reader_waiting
                        .store(false, std::sync::atomic::Ordering::Release);
                    result.map_err(|_| "tape writer 已关闭 buffer recycle channel".to_string())?
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err("tape writer 已关闭 buffer recycle channel".into());
                }
            };
            buffer.resize(block_size, 0);
            use std::io::Read;
            let read = source
                .read(&mut buffer)
                .map_err(|error| format!("读取 {description} 失败: {error}"))?;
            if read == 0 {
                allocated = allocated.saturating_sub(1);
                break;
            }
            buffer.truncate(read);
            actual_size += read as u64;
            metrics
                .bytes_read
                .fetch_add(read as u64, std::sync::atomic::Ordering::AcqRel);
            metrics
                .queued_bytes
                .fetch_add(read as u64, std::sync::atomic::Ordering::AcqRel);
            if messages.send(SourcePipelineMessage::Data(buffer)).is_err() {
                metrics
                    .queued_bytes
                    .fetch_sub(read as u64, std::sync::atomic::Ordering::AcqRel);
                return Err("tape writer 已关闭 source pipeline".into());
            }
        }
        messages
            .send(SourcePipelineMessage::FileEnd { actual_size })
            .map_err(|_| "tape writer 已关闭 source pipeline".to_string())?;
    }
    Ok(())
}

impl PlannedSource {
    fn open(&self, size: u64) -> Result<Box<dyn std::io::Read>, String> {
        match self {
            Self::Local(path) => std::fs::File::open(path)
                .map(|file| Box::new(file) as Box<dyn std::io::Read>)
                .map_err(|e| format!("打开 {} 失败: {e}", path.display())),
            Self::Pseudorandom { seed } => Ok(Box::new(PseudorandomReader::new(*seed, size))),
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Pseudorandom { seed } => format!("pseudorandom(seed={seed})"),
        }
    }
}

enum WriteSourceRequest<'a> {
    Local(&'a Path),
    LocalRoots(&'a [std::path::PathBuf]),
    Pseudorandom { size: u64, seed: u64 },
}

#[derive(Debug, Default)]
struct WritePlan {
    files: Vec<PlannedFile>,
    directories: usize,
    total_bytes: u64,
    highest_uid: u64,
}

fn plan_source_tree(
    index: &mut Index,
    source: &Path,
    target: &str,
    now: &str,
) -> Result<WritePlan, String> {
    let mut plan = WritePlan {
        highest_uid: index.highest_file_uid.unwrap_or(0),
        ..Default::default()
    };
    plan_entry(index, source, target, now, &mut plan)?;
    Ok(plan)
}

fn plan_source_roots(
    index: &mut Index,
    roots: &[std::path::PathBuf],
    destination_directory: &str,
    now: &str,
) -> Result<WritePlan, String> {
    if roots.is_empty() {
        return Err("至少选择一个 source root".into());
    }
    let mut plan = WritePlan {
        highest_uid: index.highest_file_uid.unwrap_or(0),
        ..Default::default()
    };
    for root in roots {
        let target = destination_path_for_root(root, destination_directory)?;
        plan_entry(index, root, &target, now, &mut plan)?;
    }
    Ok(plan)
}

fn destination_path_for_root(root: &Path, destination_directory: &str) -> Result<String, String> {
    let name = source_root_name(root)?;
    Ok(if destination_directory == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", destination_directory.trim_end_matches('/'))
    })
}

fn plan_entry(
    index: &mut Index,
    source: &Path,
    target: &str,
    now: &str,
    plan: &mut WritePlan,
) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(source)
        .map_err(|e| format!("读取 {} 元数据失败: {e}", source.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("第一阶段暂不写入符号链接: {}", source.display()));
    }
    let (parent_path, name) = split_target_path(target)?;
    let parent = index
        .find_directory_mut(&parent_path)
        .ok_or_else(|| format!("目标父目录不存在: /{parent_path}"))?;
    if parent.entries.iter().any(|entry| entry.name() == name) {
        return Err(format!(
            "目标已存在: {}",
            normalized_target(&parent_path, &name)
        ));
    }

    plan.highest_uid += 1;
    let uid = plan.highest_uid;
    let times = planned_times(now);
    if metadata.is_file() {
        std::fs::File::open(source).map_err(|e| format!("打开 {} 失败: {e}", source.display()))?;
        parent.entries.push(DirectoryEntry::File(FileEntry {
            name,
            fileuid: uid,
            length: metadata.len(),
            readonly: false,
            times,
            extents: Vec::new(),
            extended_attributes: Vec::new(),
            symlink_target: None,
        }));
        plan.total_bytes += metadata.len();
        plan.files.push(PlannedFile {
            source: PlannedSource::Local(source.to_path_buf()),
            target: target.to_string(),
            size: metadata.len(),
        });
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!("源路径不是普通文件或目录: {}", source.display()));
    }

    parent
        .entries
        .push(DirectoryEntry::Directory(crate::ltfs::index::Directory {
            name,
            fileuid: uid,
            readonly: false,
            times,
            extended_attributes: Vec::new(),
            entries: Vec::new(),
        }));
    plan.directories += 1;
    let mut children = std::fs::read_dir(source)
        .map_err(|e| format!("读取目录 {} 失败: {e}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取目录 {} 失败: {e}", source.display()))?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let child_name = child
            .file_name()
            .into_string()
            .map_err(|_| format!("文件名不是 UTF-8: {}", child.path().display()))?;
        let child_target = format!("{}/{}", target.trim_end_matches('/'), child_name);
        plan_entry(index, &child.path(), &child_target, now, plan)?;
    }
    Ok(())
}

fn plan_pseudorandom_file(
    index: &mut Index,
    target: &str,
    size: u64,
    seed: u64,
    now: &str,
) -> Result<WritePlan, String> {
    let (parent_path, name) = split_target_path(target)?;
    let uid = index.highest_file_uid.unwrap_or(0) + 1;
    let parent = index
        .find_directory_mut(&parent_path)
        .ok_or_else(|| format!("目标父目录不存在: /{parent_path}"))?;
    if parent.entries.iter().any(|entry| entry.name() == name) {
        return Err(format!(
            "目标已存在: {}",
            normalized_target(&parent_path, &name)
        ));
    }
    parent.entries.push(DirectoryEntry::File(FileEntry {
        name,
        fileuid: uid,
        length: size,
        readonly: false,
        times: planned_times(now),
        extents: Vec::new(),
        extended_attributes: vec![
            ExtendedAttribute {
                key: "tapecpy.test.pseudorandom.algorithm".into(),
                value: "splitmix64-le".into(),
                value_type: None,
            },
            ExtendedAttribute {
                key: "tapecpy.test.pseudorandom.seed".into(),
                value: seed.to_string(),
                value_type: None,
            },
            ExtendedAttribute {
                key: "tapecpy.test.pseudorandom.size".into(),
                value: size.to_string(),
                value_type: None,
            },
        ],
        symlink_target: None,
    }));
    Ok(WritePlan {
        files: vec![PlannedFile {
            source: PlannedSource::Pseudorandom { seed },
            target: target.to_string(),
            size,
        }],
        directories: 0,
        total_bytes: size,
        highest_uid: uid,
    })
}

/// SplitMix64：快速、确定性测试数据生成器；不用于密码学用途。
struct PseudorandomReader {
    state: u64,
    remaining: u64,
    word: [u8; 8],
    word_offset: usize,
}

impl PseudorandomReader {
    fn new(seed: u64, size: u64) -> Self {
        Self {
            state: seed,
            remaining: size,
            word: [0; 8],
            word_offset: 8,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

impl std::io::Read for PseudorandomReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = buf
            .len()
            .min(self.remaining.min(usize::MAX as u64) as usize);
        let mut offset = 0;
        while offset < len {
            if self.word_offset == self.word.len() {
                self.word = self.next_u64().to_le_bytes();
                self.word_offset = 0;
            }
            let take = (len - offset).min(self.word.len() - self.word_offset);
            buf[offset..offset + take]
                .copy_from_slice(&self.word[self.word_offset..self.word_offset + take]);
            offset += take;
            self.word_offset += take;
        }
        self.remaining -= len as u64;
        Ok(len)
    }
}

fn split_target_path(target: &str) -> Result<(String, String), String> {
    let trimmed = target.trim_matches('/');
    if trimmed.is_empty() {
        return Err("磁带目标路径不能是根目录".into());
    }
    let (parent, name) = trimmed
        .rsplit_once('/')
        .map_or(("", trimmed), |(parent, name)| (parent, name));
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!("磁带目标路径无效: {target}"));
    }
    Ok((parent.to_string(), name.to_string()))
}

fn normalized_target(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        format!("/{name}")
    } else {
        format!("/{parent}/{name}")
    }
}

fn planned_times(now: &str) -> FileTimes {
    FileTimes {
        creation_time: Some(now.into()),
        change_time: Some(now.into()),
        modify_time: Some(now.into()),
        access_time: Some(now.into()),
        backup_time: Some(now.into()),
    }
}

/// 把一个本地文件写入 LTFS 卷（数据分区追加 + index 更新）。
///
/// `tape_path` 形如 `/dir/name`；目标目录必须已存在（暂不自动建目录）。
pub fn write_file(
    drive: &TapeDrive,
    local_path: &std::path::Path,
    tape_path: &str,
) -> Result<WriteResult, String> {
    let mut ignore = |_: &WriteEvent| {};
    WriteSession::new(drive).run(local_path, tape_path, &mut ignore)
}

fn write_with_observer(
    drive: &TapeDrive,
    source: WriteSourceRequest<'_>,
    tape_path: &str,
    options: WriteOptions,
    observer: &mut dyn FnMut(&WriteEvent),
) -> Result<WriteResult, String> {
    write_with_observer_detailed(drive, source, tape_path, options, observer)
        .map_err(|error| error.to_string())
}

fn write_with_observer_detailed(
    drive: &TapeDrive,
    source: WriteSourceRequest<'_>,
    tape_path: &str,
    options: WriteOptions,
    observer: &mut dyn FnMut(&WriteEvent),
) -> Result<WriteResult, WriteFailure> {
    let mut last_event = None;
    let result = {
        let mut forwarding = |event: &WriteEvent| {
            last_event = Some(event.clone());
            observer(event);
        };
        write_with_observer_inner(drive, source, tape_path, options, &mut forwarding)
    };
    result.map_err(|message| {
        let failure = classify_write_failure(last_event.as_ref(), message);
        observer(&WriteEvent {
            phase: WritePhase::Failed,
            current_file: failure.current_file.clone(),
            files_completed: failure.files_completed,
            files_total: last_event.as_ref().map_or(0, |event| event.files_total),
            bytes_written: failure.bytes_written,
            bytes_total: last_event.as_ref().map_or(0, |event| event.bytes_total),
            partition: failure.partition,
            logical_block: failure.logical_block,
            telemetry: None,
            performance: None,
            failure: Some(failure.clone()),
        });
        failure
    })
}

fn classify_write_failure(last: Option<&WriteEvent>, message: String) -> WriteFailure {
    let phase = last.map_or(WritePhase::Preparing, |event| event.phase);
    let bytes_written = last.map_or(0, |event| event.bytes_written);
    let commit_state = if message.contains("[state:C4]") {
        WriteCommitState::CoherencyPartial
    } else {
        match phase {
            WritePhase::Preparing => WriteCommitState::NotStarted,
            WritePhase::WritingData if bytes_written == 0 => WriteCommitState::NotStarted,
            WritePhase::WritingData | WritePhase::FinalizingDataIndex => {
                WriteCommitState::DataIncomplete
            }
            WritePhase::SyncingIndexPartition => WriteCommitState::DataIndexOnly,
            WritePhase::UpdatingCoherency => WriteCommitState::IndexesWritten,
            WritePhase::Verifying | WritePhase::Completed => WriteCommitState::Committed,
            WritePhase::Failed => WriteCommitState::NotStarted,
        }
    };
    let (partition, logical_block) =
        last.map_or((None, None), |event| (event.partition, event.logical_block));
    let requires_diagnosis = matches!(
        commit_state,
        WriteCommitState::DataIncomplete
            | WriteCommitState::DataIndexOnly
            | WriteCommitState::IndexesWritten
            | WriteCommitState::CoherencyPartial
    ) || message.contains("不一致")
        || message.contains("coherency");
    WriteFailure {
        phase,
        commit_state,
        message: message.replace("[state:C4]", "").replace("[cancelled]", ""),
        current_file: last.and_then(|event| event.current_file.clone()),
        files_completed: last.map_or(0, |event| event.files_completed),
        bytes_written,
        partition,
        logical_block,
        safe_to_retry: commit_state == WriteCommitState::NotStarted && !requires_diagnosis,
        requires_diagnosis,
        cancelled: message.contains("[cancelled]"),
    }
}

fn write_with_observer_inner(
    drive: &TapeDrive,
    source: WriteSourceRequest<'_>,
    tape_path: &str,
    options: WriteOptions,
    observer: &mut dyn FnMut(&WriteEvent),
) -> Result<WriteResult, String> {
    let mut session = TapeSession::open(&drive.sg_path).map_err(|e| e.to_string())?;
    let health_before = read_drive_health_session(&mut session);
    let mut channel_telemetry = ChannelTelemetryState::new(health_before.write_channels.clone());
    let mut volume = inspect_volume_session(&mut session).map_err(|e| e.to_string())?;
    if !volume.recognized {
        return Err(volume.reason.unwrap_or_else(|| "不是 LTFS 卷".into()));
    }
    let label = volume
        .label
        .clone()
        .ok_or_else(|| "缺少 LTFS label".to_string())?;
    let mut index = volume
        .index
        .take()
        .ok_or_else(|| "没有可用的 index".to_string())?;

    validate_write_vci(&mut session, &label, &index)?;

    if index
        .volume_lock_state
        .as_deref()
        .is_some_and(|state| state != "unlocked")
    {
        return Err(format!(
            "LTFS 卷当前不可写（volumelockstate={}）",
            index.volume_lock_state.as_deref().unwrap_or_default()
        ));
    }
    let now = ltfs_time_now();
    let plan = match source {
        WriteSourceRequest::Local(local_path) => {
            plan_source_tree(&mut index, local_path, tape_path, &now)?
        }
        WriteSourceRequest::LocalRoots(local_roots) => {
            plan_source_roots(&mut index, local_roots, tape_path, &now)?
        }
        WriteSourceRequest::Pseudorandom { size, seed } => {
            plan_pseudorandom_file(&mut index, tape_path, size, seed, &now)?
        }
    };
    if let Some(expected) = options.expected_source {
        validate_write_plan_expectation(
            plan.files.len(),
            plan.directories,
            plan.total_bytes,
            expected,
        )?;
    }
    let mut progress = WriteProgress {
        observer,
        files_total: plan.files.len(),
        bytes_total: plan.total_bytes,
        files_completed: 0,
        bytes_written: 0,
        partition: None,
        logical_block: None,
    };
    progress.emit(WritePhase::Preparing, None);
    let index_write_block = volume
        .index_write_block
        .ok_or_else(|| "无法确定 index 分区刷新位置".to_string())?;
    let blocksize = if label.blocksize == 0 {
        512 * 1024
    } else {
        label.blocksize as usize
    };

    // index 分区需要在旧 index 起点覆盖写。驱动器若仍处于 append-only
    // 模式，LOCATE 可能成功但后续 WRITE 实际落在 EOD，破坏分区头部。
    session
        .ensure_write_anywhere()
        .map_err(|e| format!("无法启用 LTFS write-anywhere 模式: {e}"))?;

    // 定位数据追加位置（不依赖驱动器 EOD，参考 LTFSCopyGUI LocateToWritePosition）：
    // data 分区始终以「最新 index 文件 + filemark」结尾，latest index 的
    // previousgenerationlocation 指向该文件（mkltfs 初始 index 指向 data 分区块 5）。
    let data_last_index = index
        .previous_location
        .clone()
        .filter(|p| p.partition == label.data_partition)
        .ok_or_else(|| "index 缺少指向 data 分区的 previousgenerationlocation".to_string())?;
    let mut data_pos = locate_checked_data_append(
        &mut session,
        &data_last_index,
        &index.volume_uuid,
        index.generation,
    )?;
    let data_append = data_pos;
    progress.partition = Some(label.data_partition);
    progress.logical_block = Some(data_pos);

    // 分块写入本地文件内容。普通文件之间不写 filemark；最终由下面写入
    // data index 前的 filemark 结束数据文件区。
    let mut total = 0u64;
    let mut first_write = true;
    let mut hashes = Vec::with_capacity(plan.files.len());
    channel_telemetry.begin_data();
    let pipeline = SourcePipeline::spawn(
        plan.files.clone(),
        blocksize,
        DEFAULT_WRITE_BUFFER_BYTES,
        options.cancellation.clone().unwrap_or_default(),
    )?;
    let mut performance = WritePerformanceState::new();
    struct ActiveWriteFile {
        target: String,
        description: String,
        expected_size: u64,
        start_block: u64,
        bytes_written: u64,
        hasher: Sha256,
    }
    let mut active_file: Option<ActiveWriteFile> = None;
    while progress.files_completed < plan.files.len() {
        let Some(message) = pipeline.recv_timeout(std::time::Duration::from_millis(250))? else {
            if let Some(sample) =
                performance.sample_if_due(pipeline.snapshot(), progress.bytes_written)
            {
                progress.emit_with_performance(
                    WritePhase::WritingData,
                    active_file.as_ref().map(|file| file.target.as_str()),
                    sample,
                );
            }
            continue;
        };
        match message {
            SourcePipelineMessage::FileStart {
                target,
                description,
                expected_size,
            } => {
                if active_file.is_some() {
                    return Err("source pipeline 在上一文件结束前发送了新文件".into());
                }
                progress.emit(WritePhase::WritingData, Some(&target));
                active_file = Some(ActiveWriteFile {
                    target,
                    description,
                    expected_size,
                    start_block: data_pos,
                    bytes_written: 0,
                    hasher: Sha256::new(),
                });
            }
            SourcePipelineMessage::Data(data) => {
                let Some(file) = active_file.as_mut() else {
                    return Err("source pipeline 在 FileStart 前发送了数据".into());
                };
                if let Err(e) = session.write_record(&data) {
                    if first_write && is_end_of_data_error(&e) {
                        return Err(format!(
                            "写入被拒绝：驱动器报告的 EOD 与磁带实际内容不一致（刚用 mkltfs 格式化的磁带常见）。\
                         这可能是因为格式化后驱动器 EOD 标记未与内容同步。详情: {e}"
                        ));
                    }
                    return Err(format!("写入 {} 失败: {e}", file.description));
                }
                first_write = false;
                file.hasher.update(&data);
                file.bytes_written += data.len() as u64;
                total += data.len() as u64;
                progress.bytes_written = total;
                data_pos += 1;
                progress.logical_block = Some(data_pos);
                pipeline.recycle(data);
                if let Some(sample) =
                    performance.sample_if_due(pipeline.snapshot(), progress.bytes_written)
                {
                    progress.emit_with_performance(
                        WritePhase::WritingData,
                        Some(&file.target),
                        sample,
                    );
                }
                if options.failpoint == Some(WriteFailpoint::AfterFirstDataRecord)
                    || options.cancelpoint == Some(WriteFailpoint::AfterFirstDataRecord)
                    || options
                        .cancellation
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled)
                {
                    progress.emit(WritePhase::WritingData, Some(&file.target));
                    check_write_stop(&options, WriteFailpoint::AfterFirstDataRecord)?;
                }
                if let Some(sample) = channel_telemetry.sample_if_due(
                    &mut session,
                    label.data_partition,
                    data_pos,
                    progress.bytes_written,
                ) {
                    progress.emit_with_telemetry(
                        WritePhase::WritingData,
                        Some(&file.target),
                        Some(sample),
                    );
                }
            }
            SourcePipelineMessage::FileEnd { actual_size } => {
                let Some(file) = active_file.take() else {
                    return Err("source pipeline 在 FileStart 前发送了 FileEnd".into());
                };
                if actual_size != file.expected_size || file.bytes_written != file.expected_size {
                    return Err(format!(
                        "源文件大小在规划后发生变化：{}（计划 {} 字节，实际读取 {} 字节，实际写入 {} 字节）",
                        file.description, file.expected_size, actual_size, file.bytes_written
                    ));
                }
                let entry = index
                    .find_file_mut(&file.target)
                    .expect("文件条目已在写入前规划");
                entry.length = file.bytes_written;
                entry.extents = if file.bytes_written > 0 {
                    vec![Extent {
                        file_offset: 0,
                        partition: label.data_partition,
                        start_block: file.start_block,
                        byte_count: file.bytes_written,
                    }]
                } else {
                    Vec::new()
                };
                let sha256 = format!("{:x}", file.hasher.finalize());
                entry
                    .extended_attributes
                    .retain(|attr| !attr.key.eq_ignore_ascii_case("ltfs.hash.sha256sum"));
                entry.extended_attributes.push(ExtendedAttribute {
                    key: "ltfs.hash.sha256sum".into(),
                    value: sha256.clone(),
                    value_type: None,
                });
                hashes.push(FileHash {
                    path: file.target.clone(),
                    sha256,
                });
                progress.files_completed += 1;
                progress.emit(WritePhase::WritingData, Some(&file.target));
            }
            SourcePipelineMessage::Error(error) => return Err(error),
        }
    }
    let final_pipeline_snapshot = pipeline.snapshot();
    pipeline.join()?;
    if let Some(sample) = performance.sample(final_pipeline_snapshot, progress.bytes_written, true)
    {
        progress.emit_with_performance(WritePhase::WritingData, None, sample);
    }

    // 更新 index 元数据：generation、时间、UID（位置在下面按分区分别设置）
    index.generation += 1;
    index.highest_file_uid = Some(plan.highest_uid);
    index.update_time = now;

    // 写 data 分区 index：[FM][index][FM]（参考 WriteCurrentIndex）
    progress.emit(WritePhase::FinalizingDataIndex, None);
    session.write_filemark().map_err(|e| e.to_string())?;
    data_pos += 1;
    progress.logical_block = Some(data_pos);
    let data_index_start = data_pos;
    index.self_location = TapePos {
        partition: label.data_partition,
        startblock: data_index_start,
    };
    index.previous_location = Some(data_last_index);
    let xml = index.to_xml();
    for chunk in xml.as_bytes().chunks(blocksize) {
        session.write_record(chunk).map_err(|e| e.to_string())?;
    }
    session.write_filemark().map_err(|e| e.to_string())?;

    // 镜像到 index 分区：[FM][index][FM]（参考 RefreshIndexPartition）
    progress.emit(WritePhase::SyncingIndexPartition, None);
    check_write_stop(&options, WriteFailpoint::AfterDataIndex)?;
    session
        .locate(label.index_partition, index_write_block)
        .map_err(|e| e.to_string())?;
    progress.partition = Some(label.index_partition);
    progress.logical_block = Some(index_write_block);
    let mut idx_pos = index_write_block;
    session.write_filemark().map_err(|e| e.to_string())?;
    idx_pos += 1;
    progress.logical_block = Some(idx_pos);
    let idx_copy_start = idx_pos;
    index.self_location = TapePos {
        partition: label.index_partition,
        startblock: idx_copy_start,
    };
    index.previous_location = Some(TapePos {
        partition: label.data_partition,
        startblock: data_index_start,
    });
    let xml = index.to_xml();
    for chunk in xml.as_bytes().chunks(blocksize) {
        session.write_record(chunk).map_err(|e| e.to_string())?;
    }
    session.write_filemark().map_err(|e| e.to_string())?;

    progress.emit(WritePhase::UpdatingCoherency, None);
    update_write_volume_coherency(
        &mut session,
        &index.volume_uuid,
        index.generation,
        &[
            (label.index_partition, idx_copy_start),
            (label.data_partition, data_index_start),
        ],
        &options,
    )
    .map_err(|e| format!("两个 index 已写完，但更新 MAM VCI 失败：{e}"))?;

    if options.verification == WriteVerification::ReadBackSha256 {
        progress.emit(WritePhase::Verifying, None);
        check_write_stop(&options, WriteFailpoint::BeforeVerify)?;
        for hash in &hashes {
            progress.emit(WritePhase::Verifying, Some(&hash.path));
            verify_file_hash(&mut session, &index, hash).map_err(|e| {
                format!(
                    "写入已提交（index generation {}、MAM VCI 已更新），但写后校验失败：{e}",
                    index.generation
                )
            })?;
        }
    }

    let health_after = read_drive_health_session(&mut session);
    let health_delta = DriveHealthDelta::between(&health_before, &health_after);
    progress.emit(WritePhase::Completed, None);
    let telemetry_history = channel_telemetry.history.into_iter().collect();

    Ok(WriteResult {
        bytes: total,
        files: plan.files.len(),
        directories: plan.directories,
        file_uid: plan.highest_uid,
        generation: index.generation,
        data_start_block: data_append,
        hashes,
        verification: options.verification,
        health_before,
        health_after,
        health_delta,
        telemetry_history,
        session_worst_channel_rate: channel_telemetry.session_worst,
        telemetry_warnings: channel_telemetry.warnings,
    })
}

fn validate_write_plan_expectation(
    files: usize,
    directories: usize,
    payload_bytes: u64,
    expected: WritePlanExpectation,
) -> Result<(), String> {
    if files == expected.files
        && directories == expected.directories
        && payload_bytes == expected.payload_bytes
    {
        return Ok(());
    }
    Err(format!(
        "source 在最终确认后发生变化：计划 {} files / {} directories / {} bytes，当前 {} files / {} directories / {} bytes",
        expected.files,
        expected.directories,
        expected.payload_bytes,
        files,
        directories,
        payload_bytes
    ))
}

fn verify_file_hash(
    session: &mut TapeSession,
    index: &Index,
    expected: &FileHash,
) -> Result<(), String> {
    let file = index
        .find_file(&expected.path)
        .ok_or_else(|| format!("index 中找不到 {}", expected.path))?;
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    for extent in &file.extents {
        session
            .locate(extent.partition, extent.start_block)
            .map_err(|e| format!("定位 {} 失败: {e}", expected.path))?;
        let mut remaining = extent.byte_count;
        while remaining > 0 {
            match session
                .read_record()
                .map_err(|e| format!("回读 {} 失败: {e}", expected.path))?
            {
                ReadRecord::Data(buf) => {
                    let n = buf.len().min(remaining as usize);
                    hasher.update(&buf[..n]);
                    bytes += n as u64;
                    remaining -= n as u64;
                }
                _ => return Err(format!("回读 {} 时磁带记录意外结束", expected.path)),
            }
        }
    }
    if bytes != file.length {
        return Err(format!(
            "{} 长度不匹配：index {} 字节，回读 {} 字节",
            expected.path, file.length, bytes
        ));
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected.sha256 {
        return Err(format!(
            "{} SHA-256 不匹配：期望 {}，实际 {}",
            expected.path, expected.sha256, actual
        ));
    }
    Ok(())
}

fn validate_write_vci(
    session: &mut TapeSession,
    label: &Label,
    index: &Index,
) -> Result<(), String> {
    let data_location = index
        .previous_location
        .as_ref()
        .filter(|location| location.partition == label.data_partition)
        .ok_or_else(|| "index 缺少有效的 data previous-generation location".to_string())?;
    let expected = [
        (label.index_partition, index.self_location.startblock),
        (label.data_partition, data_location.startblock),
    ];
    let mut copies = Vec::new();
    let mut errors = Vec::new();
    for (partition, block) in expected {
        match session.read_mam_attribute(partition, mam::VOLUME_COHERENCY_INFORMATION) {
            Ok(raw) => match VolumeCoherencyInformation::parse(&raw) {
                Ok(vci) => copies.push((partition, block, vci)),
                Err(error) => errors.push(format!("P{partition} VCI 解析失败: {error}")),
            },
            Err(error) => errors.push(format!("P{partition} VCI 读取失败: {error}")),
        }
    }
    // 两份均不可用视为设备不支持 VCI；LTFS constructs 仍是事实来源。
    if copies.is_empty() {
        return Ok(());
    }
    if copies.len() != 2 {
        return Err(format!(
            "卷 coherency 不一致：只有一份可用 VCI（{}）",
            errors.join("; ")
        ));
    }
    for (partition, block, vci) in copies {
        if vci.generation != index.generation
            || vci.volume_uuid != index.volume_uuid
            || vci.block != block
        {
            return Err(format!(
                "卷 coherency 不一致：P{partition} VCI 指向 gen {} block {} uuid {}，期望 gen {} block {} uuid {}",
                vci.generation,
                vci.block,
                vci.volume_uuid,
                index.generation,
                block,
                index.volume_uuid
            ));
        }
    }
    Ok(())
}

/// 根据 index 分区指向的 data index 确定安全追加位置。
///
/// 除了验证被引用的 index，还继续读到物理 EOD。如果引用 index 的 filemark
/// 后存在任何数据、filemark 或更新 index，说明上一次写入可能已更新 data
/// 分区但尚未同步 index 分区；此时必须拒绝写入，避免从旧位置覆盖数据。
fn locate_checked_data_append(
    session: &mut TapeSession,
    referenced: &TapePos,
    volume_uuid: &str,
    index_generation: u64,
) -> Result<u64, String> {
    session
        .locate(referenced.partition, referenced.startblock)
        .map_err(|e| e.to_string())?;

    let mut group = Vec::new();
    let mut group_is_xml = None;
    let mut referenced_end = None;
    let mut saw_tail = false;
    let mut newer_generation = None;

    loop {
        match session.read_record().map_err(|e| e.to_string())? {
            ReadRecord::Data(buf) => {
                if referenced_end.is_some() {
                    saw_tail = true;
                }
                let is_xml = *group_is_xml.get_or_insert_with(|| buf.starts_with(b"<?xml"));
                if is_xml {
                    group.extend_from_slice(&buf);
                }
            }
            ReadRecord::Filemark => {
                let after_mark = session.read_position().map_err(|e| e.to_string())?.block;
                if referenced_end.is_none() {
                    let text = std::str::from_utf8(&group)
                        .map_err(|_| "data 分区引用位置不是 UTF-8 index".to_string())?;
                    let data_index = Index::parse_xml(text)
                        .map_err(|e| format!("data 分区引用位置不是有效 index: {e}"))?;
                    if data_index.self_location != *referenced {
                        return Err(format!(
                            "data index 自身位置不匹配：引用 p{}b{}，index 声明 p{}b{}",
                            referenced.partition,
                            referenced.startblock,
                            data_index.self_location.partition,
                            data_index.self_location.startblock
                        ));
                    }
                    if data_index.volume_uuid != volume_uuid {
                        return Err("data index 的 volume UUID 与 index 分区不一致".into());
                    }
                    if data_index.generation > index_generation {
                        return Err(format!(
                            "data index generation {} 新于 index 分区 generation {}",
                            data_index.generation, index_generation
                        ));
                    }
                    referenced_end = Some(after_mark);
                } else {
                    saw_tail = true;
                    if let Ok(text) = std::str::from_utf8(&group)
                        && let Ok(idx) = Index::parse_xml(text)
                    {
                        newer_generation =
                            Some(newer_generation.map_or(idx.generation, |current: u64| {
                                current.max(idx.generation)
                            }));
                    }
                }
                group.clear();
                group_is_xml = None;
            }
            ReadRecord::Eod => break,
        }
    }

    let append = referenced_end.ok_or_else(|| {
        format!(
            "data 分区 index p{}b{} 后没有 filemark",
            referenced.partition, referenced.startblock
        )
    })?;
    if saw_tail {
        let detail = newer_generation
            .map(|generation| format!("，检测到 generation {generation} data index"))
            .unwrap_or_default();
        return Err(format!(
            "data/index 分区不一致：index 分区引用的 data index 后仍有内容{detail}；拒绝写入以避免覆盖，请先恢复卷一致性"
        ));
    }

    session
        .locate(referenced.partition, append)
        .map_err(|e| e.to_string())?;
    Ok(append)
}

/// 判断 SCSI 错误是否为「END OF DATA」（驱动器拒绝写入其 EOD 之后）。
fn is_end_of_data_error(e: &device::Error) -> bool {
    match e {
        device::Error::Scsi { sense, .. } => crate::device::scsi::parse_sense(sense)
            .is_some_and(|s| s.key == 0x08 || (s.asc == 0x00 && s.ascq == 0x05)),
        _ => false,
    }
}

/// 当前 UTC 时间，LTFS ISO 8601 格式（`YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`）。
pub(crate) fn ltfs_time_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as libc::time_t;
    let nanos = now.subsec_nanos();

    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: tm 指向有效内存；UTC 转换无副作用。
    unsafe {
        libc::gmtime_r(&secs, &mut tm);
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        nanos
    )
}

/// 装载当前介质（磁带已推入但驱动器尚未识别时使用）。
pub fn load_tape(drive: &TapeDrive) -> Result<(), device::Error> {
    let mut session = TapeSession::open(&drive.sg_path)?;
    session.load()
}

/// 弹出介质（eject）。
pub fn unload_tape(drive: &TapeDrive) -> Result<(), device::Error> {
    let mut session = TapeSession::open(&drive.sg_path)?;
    session.unload()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ltfs::index::{Directory, TapePos};

    #[test]
    fn tui_media_lifecycle_distinguishes_unthreaded_cartridge() {
        let no_media = device::MediaInfo {
            presence: device::MediaPresence::NotLoaded,
            ..Default::default()
        };
        assert_eq!(
            media_lifecycle(Some(&no_media)),
            MediaLifecycle::NoMediaDetected
        );

        let present = device::MediaInfo {
            presence: device::MediaPresence::NotLoaded,
            mam: Some(device::MamInfo::default()),
            ..Default::default()
        };
        assert_eq!(
            media_lifecycle(Some(&present)),
            MediaLifecycle::PresentUnthreaded
        );

        let loaded = device::MediaInfo {
            presence: device::MediaPresence::Loaded,
            ..Default::default()
        };
        assert_eq!(
            media_lifecycle(Some(&loaded)),
            MediaLifecycle::LoadedThreaded
        );
        assert_eq!(media_lifecycle(None), MediaLifecycle::Unknown);
    }

    #[test]
    fn tui_channel_tracker_keeps_last_sample_when_refresh_fails() {
        let mut tracker = ChannelTelemetryTracker::default();
        let baseline = DriveHealth {
            write_channels: Some(vec![device::channel_error::ChannelCounters {
                c1_errors: 10,
                ccps: 100,
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert!(tracker.observe(Some(&baseline), "t0").rates.is_empty());

        let sample = DriveHealth {
            write_channels: Some(vec![device::channel_error::ChannelCounters {
                c1_errors: 20,
                ccps: 200,
                ..Default::default()
            }]),
            ..Default::default()
        };
        let current = tracker.observe(Some(&sample), "t1");
        assert_eq!(current.rates.len(), 1);
        assert_eq!(current.last_success.as_deref(), Some("t1"));
        assert!(!current.stale);

        let stale = tracker.mark_error("temporary failure");
        assert_eq!(stale.rates, current.rates);
        assert_eq!(stale.last_success.as_deref(), Some("t1"));
        assert!(stale.stale);
    }

    fn empty_index() -> Index {
        Index {
            version: "2.4.0".into(),
            creator: "test".into(),
            volume_uuid: "00000000-0000-0000-0000-000000000000".into(),
            generation: 1,
            update_time: "2026-08-09T00:00:00.000000000Z".into(),
            self_location: TapePos {
                partition: 0,
                startblock: 5,
            },
            previous_location: Some(TapePos {
                partition: 1,
                startblock: 5,
            }),
            allow_policy_update: false,
            volume_lock_state: Some("unlocked".into()),
            highest_file_uid: Some(1),
            root: Directory {
                name: "test".into(),
                fileuid: 1,
                ..Default::default()
            },
        }
    }

    #[test]
    fn plans_directory_tree_in_stable_order() {
        let root = std::env::temp_dir().join(format!(
            "tapecpy-plan-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("b.bin"), b"bb").unwrap();
        std::fs::write(root.join("a.bin"), b"a").unwrap();
        std::fs::write(root.join("sub/c.bin"), b"ccc").unwrap();

        let mut index = empty_index();
        let plan = plan_source_tree(&mut index, &root, "/batch", "now").unwrap();
        assert_eq!(plan.directories, 2);
        assert_eq!(plan.total_bytes, 6);
        assert_eq!(plan.files.len(), 3);
        assert_eq!(plan.files[0].target, "/batch/a.bin");
        assert_eq!(plan.files[1].target, "/batch/b.bin");
        assert_eq!(plan.files[2].target, "/batch/sub/c.bin");
        assert!(index.find_directory("/batch/sub").is_some());
        assert!(index.find_file("/batch/sub/c.bin").is_some());
        assert_eq!(plan.highest_uid, 6);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn planning_rejects_existing_target_before_execution() {
        let root =
            std::env::temp_dir().join(format!("tapecpy-plan-conflict-{}", std::process::id()));
        let _ = std::fs::remove_file(&root);
        std::fs::write(&root, b"x").unwrap();
        let mut index = empty_index();
        index.root.entries.push(DirectoryEntry::File(FileEntry {
            name: "exists.bin".into(),
            ..Default::default()
        }));
        let error = plan_source_tree(&mut index, &root, "/exists.bin", "now").unwrap_err();
        assert!(error.contains("目标已存在"));
        std::fs::remove_file(root).unwrap();
    }

    #[test]
    fn write_destination_uses_source_basename_and_rejects_conflicts() {
        let mut index = empty_index();
        index
            .root
            .entries
            .push(DirectoryEntry::Directory(Directory {
                name: "archive".into(),
                ..Directory::default()
            }));
        assert_eq!(
            plan_write_destination(&index, Path::new("/mnt/nfs/session01"), "/archive").unwrap(),
            "/archive/session01"
        );
        index
            .find_directory_mut("/archive")
            .unwrap()
            .entries
            .push(DirectoryEntry::File(FileEntry {
                name: "session01".into(),
                ..FileEntry::default()
            }));
        assert!(
            plan_write_destination(&index, Path::new("/mnt/nfs/session01"), "/archive")
                .unwrap_err()
                .contains("已存在")
        );
    }

    #[test]
    fn multiple_roots_share_one_plan_and_reject_duplicate_target_names() {
        let base = std::env::temp_dir().join(format!(
            "tapecpy-multi-plan-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let left = base.join("left");
        let right = base.join("right");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        std::fs::write(left.join("a.bin"), b"aa").unwrap();
        std::fs::write(right.join("b.bin"), b"bbb").unwrap();

        let mut index = empty_index();
        let roots = vec![left.clone(), right.clone()];
        let targets = validate_write_destinations(&index, &roots, "/").unwrap();
        assert_eq!(targets, vec!["/left", "/right"]);
        let plan = plan_source_roots(&mut index, &roots, "/", "now").unwrap();
        assert_eq!(plan.files.len(), 2);
        assert_eq!(plan.directories, 2);
        assert_eq!(plan.total_bytes, 5);
        assert!(index.find_file("/left/a.bin").is_some());
        assert!(index.find_file("/right/b.bin").is_some());

        let duplicate_parent = base.join("duplicate");
        let duplicate_left = duplicate_parent.join("one/same");
        let duplicate_right = duplicate_parent.join("two/same");
        std::fs::create_dir_all(&duplicate_left).unwrap();
        std::fs::create_dir_all(&duplicate_right).unwrap();
        let error =
            validate_write_destinations(&empty_index(), &[duplicate_left, duplicate_right], "/")
                .unwrap_err();
        assert!(error.contains("同一个 LTFS target"));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn source_scan_builds_stable_progress_denominator() {
        let root = std::env::temp_dir().join(format!(
            "tapecpy-source-scan-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("b.bin"), b"bb").unwrap();
        std::fs::write(root.join("a.bin"), b"a").unwrap();
        std::fs::write(root.join("sub/c.bin"), b"ccc").unwrap();

        let plan = scan_source_roots(std::slice::from_ref(&root)).unwrap();
        assert_eq!(plan.files.len(), 3);
        assert_eq!(plan.directories_total, 2);
        assert_eq!(plan.payload_bytes, 6);
        assert!(plan.files[0].path.ends_with("a.bin"));
        assert!(plan.files[1].path.ends_with("b.bin"));
        assert!(plan.files[2].path.ends_with("sub/c.bin"));
        assert_eq!(plan.files[2].size, 3);

        let overlap = scan_source_roots(&[root.clone(), root.join("sub")]).unwrap_err();
        assert!(overlap.contains("互相包含"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_scan_obeys_cancellation_before_io_walk() {
        let token = CancellationToken::default();
        token.request_cancel();
        let mut observer = |_: &SourceScanProgress| {};
        let error = scan_source_roots_with_observer(&[std::env::temp_dir()], &token, &mut observer)
            .unwrap_err();
        assert!(error.contains("cancelled"));
    }

    #[test]
    fn capacity_assessment_has_warning_blocked_and_unknown_states() {
        // Capacity input is MiB, so use matching byte units for threshold tests.
        let mib = 1024 * 1024;
        let normal = assess_write_capacity(90 * mib, Some(100), "t0");
        assert_eq!(normal.status, CapacityStatus::Normal);
        assert_eq!(normal.source, Some(CapacitySource::MamRemainingCapacity));

        let warning = assess_write_capacity(91 * mib, Some(100), "t1");
        assert_eq!(warning.status, CapacityStatus::WarningAboveNinetyPercent);
        assert!(warning.planned_fraction.unwrap() > 0.9);

        let blocked = assess_write_capacity(101 * mib, Some(100), "t2");
        assert_eq!(blocked.status, CapacityStatus::BlockedInsufficient);

        let unknown = assess_write_capacity(1, None, "t3");
        assert_eq!(unknown.status, CapacityStatus::Unknown);
        assert_eq!(unknown.planned_fraction, None);
    }

    #[test]
    fn write_progress_events_carry_stable_totals_and_progress() {
        let mut events = Vec::new();
        {
            let mut observer = |event: &WriteEvent| events.push(event.clone());
            let mut progress = WriteProgress {
                observer: &mut observer,
                files_total: 2,
                bytes_total: 30,
                files_completed: 0,
                bytes_written: 0,
                partition: None,
                logical_block: None,
            };
            progress.emit(WritePhase::Preparing, None);
            progress.emit(WritePhase::WritingData, Some("/a"));
            progress.files_completed = 1;
            progress.bytes_written = 10;
            progress.emit(WritePhase::WritingData, Some("/a"));
            progress.emit(WritePhase::FinalizingDataIndex, None);
            progress.emit(WritePhase::SyncingIndexPartition, None);
            progress.files_completed = 2;
            progress.bytes_written = 30;
            progress.emit(WritePhase::Completed, None);
        }

        assert_eq!(events.first().unwrap().phase, WritePhase::Preparing);
        assert_eq!(events.last().unwrap().phase, WritePhase::Completed);
        assert_eq!(events[2].files_completed, 1);
        assert_eq!(events[2].bytes_written, 10);
        assert!(events.iter().all(|event| event.files_total == 2));
        assert!(events.iter().all(|event| event.bytes_total == 30));
    }

    #[test]
    fn write_failure_classifies_commit_boundaries() {
        let event = |phase, bytes_written| WriteEvent {
            phase,
            current_file: Some("/test.bin".into()),
            files_completed: 0,
            files_total: 1,
            bytes_written,
            bytes_total: 1024,
            partition: Some(1),
            logical_block: Some(7),
            telemetry: None,
            performance: None,
            failure: None,
        };
        let cases = [
            (WritePhase::Preparing, 0, WriteCommitState::NotStarted),
            (WritePhase::WritingData, 1, WriteCommitState::DataIncomplete),
            (
                WritePhase::FinalizingDataIndex,
                1024,
                WriteCommitState::DataIncomplete,
            ),
            (
                WritePhase::SyncingIndexPartition,
                1024,
                WriteCommitState::DataIndexOnly,
            ),
            (
                WritePhase::UpdatingCoherency,
                1024,
                WriteCommitState::IndexesWritten,
            ),
            (WritePhase::Verifying, 1024, WriteCommitState::Committed),
        ];
        for (phase, bytes, expected) in cases {
            let event = event(phase, bytes);
            let failure = classify_write_failure(Some(&event), "injected".into());
            assert_eq!(failure.commit_state, expected);
            assert_eq!(
                failure.safe_to_retry,
                expected == WriteCommitState::NotStarted
            );
            assert_eq!(
                failure.requires_diagnosis,
                !matches!(
                    expected,
                    WriteCommitState::NotStarted | WriteCommitState::Committed
                )
            );
        }
    }

    #[test]
    fn partial_vci_failure_requires_diagnosis() {
        let event = WriteEvent {
            phase: WritePhase::UpdatingCoherency,
            current_file: None,
            files_completed: 1,
            files_total: 1,
            bytes_written: 10,
            bytes_total: 10,
            partition: Some(0),
            logical_block: Some(5),
            telemetry: None,
            performance: None,
            failure: None,
        };
        let failure = classify_write_failure(Some(&event), "[state:C4]second VCI failed".into());
        assert_eq!(failure.commit_state, WriteCommitState::CoherencyPartial);
        assert!(failure.requires_diagnosis);
        assert_eq!(failure.message, "second VCI failed");
    }

    #[test]
    fn cancellation_token_is_distinct_from_failure() {
        let token = CancellationToken::default();
        assert!(!token.is_cancelled());
        token.request_cancel();
        assert!(token.is_cancelled());
        let options = WriteOptions {
            expected_source: None,
            cancellation: Some(token),
            ..WriteOptions::default()
        };
        let error = check_write_stop(&options, WriteFailpoint::AfterDataIndex).unwrap_err();
        let failure = classify_write_failure(None, error);
        assert!(failure.cancelled);
    }

    #[test]
    fn write_plan_expectation_detects_source_changes() {
        let expected = WritePlanExpectation {
            files: 2,
            directories: 1,
            payload_bytes: 1024,
        };
        validate_write_plan_expectation(2, 1, 1024, expected).unwrap();
        let error = validate_write_plan_expectation(2, 1, 1025, expected).unwrap_err();
        assert!(error.contains("最终确认后发生变化"));
    }

    #[test]
    fn mountinfo_marks_nfs_and_cifs_and_decodes_paths() {
        let mounts = parse_mountinfo(
            "36 25 0:32 / /mnt/local\\040disk rw - ext4 /dev/sda1 rw\n\
             37 25 0:33 / /mnt/archive rw - nfs4 nas:/archive rw\n\
             38 25 0:34 / /mnt/media rw - cifs //server/media rw\n",
        )
        .unwrap();
        assert_eq!(mounts[0].filesystem_type, "nfs4");
        assert!(mounts[0].network);
        assert_eq!(mounts[1].filesystem_type, "cifs");
        assert!(mounts[1].network);
        assert_eq!(mounts[2].mount_point, Path::new("/mnt/local disk"));
        assert!(!mounts[2].network);
    }

    #[test]
    fn directory_browser_is_shallow_and_directories_sort_first() {
        let root = std::env::temp_dir().join(format!(
            "tapecpy-browser-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.bin"), [1_u8; 4]).unwrap();
        std::fs::create_dir(root.join("z-dir")).unwrap();
        std::fs::write(root.join("z-dir/hidden.bin"), [2_u8; 8]).unwrap();
        let entries = browse_directory(&root).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, BrowserEntryKind::Directory);
        assert_eq!(entries[1].size, Some(4));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn index_candidate(partition: u8, block: u64, generation: u64) -> IndexCandidateDiagnostic {
        let mut index = empty_index();
        index.generation = generation;
        index.self_location = TapePos {
            partition,
            startblock: block,
        };
        IndexCandidateDiagnostic {
            physical_partition: partition,
            actual_start_block: block,
            byte_len: 100,
            index: Some(index),
            parse_error: None,
        }
    }

    fn test_vci(partition: u8, block: u64, generation: u64) -> (u8, VolumeCoherencyInformation) {
        (
            partition,
            VolumeCoherencyInformation {
                vcr: vec![1],
                generation,
                block,
                volume_uuid: "00000000-0000-0000-0000-000000000000".into(),
                acsi_version: 1,
            },
        )
    }

    #[test]
    fn consistency_requires_matching_index_and_vci_copies() {
        let candidates = vec![index_candidate(0, 5, 4), index_candidate(1, 9, 4)];
        let vci = vec![test_vci(0, 5, 4), test_vci(1, 9, 4)];
        assert_eq!(
            classify_volume_consistency(
                Some("00000000-0000-0000-0000-000000000000"),
                &candidates,
                &vci
            ),
            (VolumeConsistency::Healthy, true)
        );

        let mut split = candidates.clone();
        split[1].index.as_mut().unwrap().generation = 3;
        assert_eq!(
            classify_volume_consistency(None, &split, &vci).0,
            VolumeConsistency::DivergentIndexes
        );

        let stale_vci = vec![test_vci(0, 5, 3), test_vci(1, 9, 3)];
        assert_eq!(
            classify_volume_consistency(None, &candidates, &stale_vci).0,
            VolumeConsistency::StaleVci
        );

        let split_vci = vec![test_vci(0, 5, 4), test_vci(1, 9, 3)];
        assert_eq!(
            classify_volume_consistency(None, &candidates, &split_vci).0,
            VolumeConsistency::DivergentVci
        );
    }

    #[test]
    fn consistency_rejects_foreign_missing_and_mislocated_indexes() {
        let candidates = vec![index_candidate(0, 5, 4), index_candidate(1, 9, 4)];
        assert_eq!(
            classify_volume_consistency(Some("foreign"), &candidates, &[]).0,
            VolumeConsistency::ForeignIndex
        );
        assert_eq!(
            classify_volume_consistency(None, &candidates[..1], &[]).0,
            VolumeConsistency::IndexCopyMissing
        );
        let mut misplaced = candidates.clone();
        misplaced[0].actual_start_block = 6;
        assert_eq!(
            classify_volume_consistency(None, &misplaced, &[]).0,
            VolumeConsistency::InvalidIndexLocation
        );
        assert_eq!(
            classify_volume_consistency(None, &[], &[]).0,
            VolumeConsistency::NoUsableIndex
        );
        assert_eq!(
            classify_volume_consistency(None, &candidates, &[]),
            (VolumeConsistency::MamUnavailable, true)
        );

        let mut tail = candidates.clone();
        tail.push(IndexCandidateDiagnostic {
            physical_partition: 1,
            actual_start_block: 10,
            byte_len: 512,
            index: None,
            parse_error: Some("ordinary data".into()),
        });
        assert_eq!(
            classify_volume_consistency(None, &tail, &[]).0,
            VolumeConsistency::UnindexedTail
        );
    }

    #[test]
    fn health_delta_uses_session_baseline_and_detects_counter_reset() {
        let before = DriveHealth {
            write_errors: Some(device::log::ErrorCounters {
                total_corrected: Some(100),
                uncorrected: Some(2),
                ..Default::default()
            }),
            read_errors: Some(device::log::ErrorCounters {
                total_corrected: Some(50),
                uncorrected: Some(4),
                ..Default::default()
            }),
            ..Default::default()
        };
        let after = DriveHealth {
            write_errors: Some(device::log::ErrorCounters {
                total_corrected: Some(107),
                uncorrected: Some(2),
                ..Default::default()
            }),
            read_errors: Some(device::log::ErrorCounters {
                total_corrected: Some(3),
                uncorrected: Some(5),
                ..Default::default()
            }),
            tape_alerts: vec![3, 20],
            ..Default::default()
        };
        let delta = DriveHealthDelta::between(&before, &after);
        assert_eq!(delta.corrected_write_errors, Some(7));
        assert_eq!(delta.hard_write_errors, Some(0));
        assert_eq!(delta.corrected_read_errors, None);
        assert_eq!(delta.hard_read_errors, Some(1));
        assert_eq!(delta.active_tape_alerts, vec![3, 20]);
    }

    #[test]
    fn pseudorandom_stream_is_bounded_and_reproducible() {
        use std::io::Read;

        let mut first = Vec::new();
        let mut second = Vec::new();
        PseudorandomReader::new(42, 19)
            .read_to_end(&mut first)
            .unwrap();
        PseudorandomReader::new(42, 19)
            .read_to_end(&mut second)
            .unwrap();
        assert_eq!(first.len(), 19);
        assert_eq!(first, second);
        assert_ne!(first, vec![0; 19]);

        let mut chunked = PseudorandomReader::new(42, 19);
        let mut pieces = Vec::new();
        let mut three = [0u8; 3];
        loop {
            let n = chunked.read(&mut three).unwrap();
            if n == 0 {
                break;
            }
            pieces.extend_from_slice(&three[..n]);
        }
        assert_eq!(first, pieces);
    }

    #[test]
    fn source_pipeline_preserves_file_boundaries_and_recycles_bounded_buffers() {
        let files = vec![
            PlannedFile {
                source: PlannedSource::Pseudorandom { seed: 1 },
                target: "/one".into(),
                size: 10,
            },
            PlannedFile {
                source: PlannedSource::Pseudorandom { seed: 2 },
                target: "/two".into(),
                size: 7,
            },
        ];
        let pipeline = SourcePipeline::spawn(files, 4, 8, CancellationToken::default()).unwrap();
        let mut events = Vec::new();
        loop {
            match pipeline.recv() {
                Ok(SourcePipelineMessage::FileStart { target, .. }) => {
                    events.push(format!("start:{target}"));
                }
                Ok(SourcePipelineMessage::Data(data)) => {
                    events.push(format!("data:{}", data.len()));
                    pipeline.recycle(data);
                }
                Ok(SourcePipelineMessage::FileEnd { actual_size }) => {
                    events.push(format!("end:{actual_size}"));
                    if actual_size == 7 {
                        break;
                    }
                }
                Ok(SourcePipelineMessage::Error(error)) => panic!("{error}"),
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(
            events,
            [
                "start:/one",
                "data:4",
                "data:4",
                "data:2",
                "end:10",
                "start:/two",
                "data:4",
                "data:3",
                "end:7",
            ]
        );
        let snapshot = pipeline.snapshot();
        assert_eq!(snapshot.bytes_read, 17);
        assert_eq!(snapshot.queued_bytes, 0);
        pipeline.join().unwrap();
    }

    #[test]
    fn source_pipeline_reports_cancellation_before_opening_a_file() {
        let cancellation = CancellationToken::default();
        cancellation.request_cancel();
        let pipeline = SourcePipeline::spawn(
            vec![PlannedFile {
                source: PlannedSource::Pseudorandom { seed: 1 },
                target: "/cancelled".into(),
                size: 8,
            }],
            4,
            8,
            cancellation,
        )
        .unwrap();
        let SourcePipelineMessage::Error(error) = pipeline.recv().unwrap() else {
            panic!("expected cancellation error")
        };
        assert!(error.contains("[cancelled]"));
        pipeline.join().unwrap();
    }

    #[test]
    fn source_pipeline_preserves_source_error_identity() {
        let missing = std::env::temp_dir().join(format!(
            "tapecpy-pipeline-missing-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let pipeline = SourcePipeline::spawn(
            vec![PlannedFile {
                source: PlannedSource::Local(missing.clone()),
                target: "/missing".into(),
                size: 8,
            }],
            4,
            8,
            CancellationToken::default(),
        )
        .unwrap();
        assert!(matches!(
            pipeline.recv().unwrap(),
            SourcePipelineMessage::FileStart { .. }
        ));
        let SourcePipelineMessage::Error(error) = pipeline.recv().unwrap() else {
            panic!("expected source error")
        };
        assert!(error.contains("打开"));
        assert!(error.contains(&missing.display().to_string()));
        pipeline.join().unwrap();
    }

    #[test]
    fn source_pipeline_applies_bounded_backpressure() {
        let pipeline = SourcePipeline::spawn(
            vec![PlannedFile {
                source: PlannedSource::Pseudorandom { seed: 9 },
                target: "/large".into(),
                size: 1_024,
            }],
            4,
            8,
            CancellationToken::default(),
        )
        .unwrap();
        assert!(matches!(
            pipeline.recv().unwrap(),
            SourcePipelineMessage::FileStart { .. }
        ));
        for _ in 0..100 {
            if pipeline.snapshot().reader_waiting {
                break;
            }
            std::thread::yield_now();
        }
        let snapshot = pipeline.snapshot();
        assert!(snapshot.queued_bytes <= snapshot.capacity_bytes);
        assert!(snapshot.reader_waiting);
        drop(pipeline);
    }

    #[test]
    fn telemetry_history_rolls_but_session_worst_survives() {
        let mut state = ChannelTelemetryState::new(None);
        for i in 0..CHANNEL_HISTORY_CAPACITY + 5 {
            let rate = if i == 0 { -2.5 } else { -6.0 };
            state.record_sample(ChannelTelemetrySample {
                elapsed_millis: i as u64 * 5_000,
                timestamp: format!("sample-{i}"),
                partition: 1,
                logical_block: i as u64,
                throughput_bytes_per_second: i as f64 * 1024.0,
                channel_rates: vec![device::channel_error::ChannelRate {
                    channel: 3,
                    log10_bit_error_rate: Some(rate),
                    ccp_advanced: true,
                }],
                worst_rate: Some(rate),
            });
        }
        assert_eq!(state.history.len(), CHANNEL_HISTORY_CAPACITY);
        assert_eq!(state.history.front().unwrap().logical_block, 5);
        let worst = state.session_worst.unwrap();
        assert_eq!(worst.rate, -2.5);
        assert_eq!(worst.logical_block, 0);
    }

    #[test]
    fn telemetry_throughput_is_interval_payload_rate() {
        assert_eq!(
            payload_throughput(10 * 1024 * 1024, std::time::Duration::from_secs(5),),
            2.0 * 1024.0 * 1024.0
        );
        assert_eq!(payload_throughput(123, std::time::Duration::ZERO), 0.0);
    }

    #[test]
    fn initial_format_image_is_consistent_across_partitions() {
        let options = FormatOptions::new("E6008A", "archive & test");
        let image = build_initial_format_image(
            &options,
            "11111111-2222-4333-8444-555555555555",
            "2026-08-09T13:00:00.000000000Z",
        )
        .unwrap();

        assert_eq!(AnsiLabel::parse(&image.ansi).unwrap().barcode, "E6008A");
        let data_label = Label::parse_xml(&image.data_label_xml).unwrap();
        let index_label = Label::parse_xml(&image.index_label_xml).unwrap();
        assert_eq!(data_label.volume_uuid, index_label.volume_uuid);
        assert_eq!(data_label.this_partition, 1);
        assert_eq!(index_label.this_partition, 0);

        let data_index = Index::parse_xml(&image.data_index_xml).unwrap();
        let index_index = Index::parse_xml(&image.index_index_xml).unwrap();
        assert_eq!(data_index.generation, 1);
        assert_eq!(
            data_index.self_location,
            TapePos {
                partition: 1,
                startblock: 5
            }
        );
        assert_eq!(data_index.previous_location, None);
        assert_eq!(
            index_index.self_location,
            TapePos {
                partition: 0,
                startblock: 5
            }
        );
        assert_eq!(
            index_index.previous_location,
            Some(TapePos {
                partition: 1,
                startblock: 5
            })
        );
        assert_eq!(index_index.volume_name(), Some("archive & test"));
        assert_eq!(
            Index::parse_xml(&index_index.to_xml()).unwrap(),
            index_index
        );
    }

    #[test]
    fn format_options_reject_unsafe_identifiers_before_device_access() {
        assert!(FormatOptions::new("short", "volume").validate().is_err());
        assert!(FormatOptions::new("E6008A", "").validate().is_err());
        assert!(
            FormatOptions::new("E6008A", "bad\nname")
                .validate()
                .is_err()
        );
    }
}
