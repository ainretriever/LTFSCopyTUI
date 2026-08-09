//! Application 层：用户操作与工作流编排。
//!
//! Milestone 0/1 包含设备发现、选择与介质检查。后续的 LTFS 格式化、
//! 写入等工作流都从这里编排，Presentation 层不得直接操作设备。

use std::path::Path;

use crate::device::tape::{LongEraseStatus, MamAttributeFormat, ReadRecord, TapeSession};
use crate::device::{self, TapeDrive};
use crate::ltfs::index::{DirectoryEntry, Extent, FileEntry, FileTimes, Index, TapePos};
use crate::ltfs::label::{AnsiLabel, Label};

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
    /// index 分区中最新 index 前的 filemark 块号（刷新 index 时的覆盖起点）。
    pub index_write_block: Option<u64>,
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
        write_format_mam(&mut tape, options, &format_time)?;

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
        write_initial_vci(&mut tape, &volume_uuid)?;
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
) -> Result<(), String> {
    let app_version = env!("CARGO_PKG_VERSION");
    let written = format_time
        .chars()
        .filter(char::is_ascii_digit)
        .take(12)
        .collect::<String>();
    let attributes = [
        (0x0800, MamAttributeFormat::Ascii, padded_ascii("OPEN", 8)),
        (
            0x0801,
            MamAttributeFormat::Ascii,
            padded_ascii("tapecpy", 32),
        ),
        (
            0x0802,
            MamAttributeFormat::Ascii,
            padded_ascii(app_version, 8),
        ),
        (
            0x0803,
            MamAttributeFormat::Text,
            padded_bytes(options.volume_name.as_bytes(), 160, 0),
        ),
        (0x0805, MamAttributeFormat::Binary, vec![0x81]),
        (
            0x0806,
            MamAttributeFormat::Ascii,
            padded_ascii(&options.barcode, 32),
        ),
        (0x080b, MamAttributeFormat::Ascii, padded_ascii("2.4.0", 16)),
        (
            0x0804,
            MamAttributeFormat::Ascii,
            padded_ascii(&written, 12),
        ),
    ];
    for (id, format, value) in attributes {
        tape.write_mam_attribute(0, id, format, &value)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn padded_ascii(value: &str, len: usize) -> Vec<u8> {
    padded_bytes(value.as_bytes(), len, b' ')
}

fn padded_bytes(value: &[u8], len: usize, fill: u8) -> Vec<u8> {
    let mut out = vec![fill; len];
    let n = value.len().min(len);
    out[..n].copy_from_slice(&value[..n]);
    out
}

fn write_initial_vci(tape: &mut TapeSession, volume_uuid: &str) -> Result<(), String> {
    // LTFSCopyGUI WriteVCI 先用 WRITE FILEMARK count=0 flush，确保刚写完的
    // index/filemark 已落带且 Volume Change Reference 不再变化。
    tape.flush().map_err(|e| e.to_string())?;
    let vcr = tape
        .read_mam_attribute(0, 0x0009)
        .map_err(|e| e.to_string())?;
    let mut vcr_u64 = [0u8; 8];
    let copy = vcr.len().min(8);
    vcr_u64[8 - copy..].copy_from_slice(&vcr[vcr.len() - copy..]);
    for (partition, block) in [(0u8, 5u64), (1u8, 5u64)] {
        let mut value = vec![0u8; 70];
        value[0] = 8;
        value[1..9].copy_from_slice(&vcr_u64);
        value[9..17].copy_from_slice(&1u64.to_be_bytes());
        value[17..25].copy_from_slice(&block.to_be_bytes());
        value[25..27].copy_from_slice(&43u16.to_be_bytes());
        value[27..32].copy_from_slice(b"LTFS\0");
        let uuid = volume_uuid.as_bytes();
        value[32..32 + uuid.len().min(36)].copy_from_slice(&uuid[..uuid.len().min(36)]);
        value[69] = 1;
        tape.write_mam_attribute(partition, 0x080c, MamAttributeFormat::Binary, &value)
            .map_err(|e| e.to_string())?;
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
                if !records.is_empty() {
                    if let Ok(text) = std::str::from_utf8(&records) {
                        if let Ok(idx) = Index::parse_xml(text) {
                            last_valid_file_start = Some(file_start);
                            latest = Some(idx);
                        }
                    }
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

/// 检查一台磁带机上的 LTFS 卷（Milestone 2）。
///
/// 流程：读取两个物理分区的 label → 确定 index 分区 → 扫描最新 index。
pub fn inspect_volume(drive: &TapeDrive) -> Result<VolumeInfo, device::Error> {
    let mut session = TapeSession::open(&drive.sg_path)?;
    inspect_volume_session(&mut session)
}

/// 用已有会话检查 LTFS 卷（供需要继续使用同一会话的命令复用）。
fn inspect_volume_session(mut session: &mut TapeSession) -> Result<VolumeInfo, device::Error> {
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
    let mut session = TapeSession::open(&drive.sg_path).map_err(|e| e.to_string())?;
    let volume = inspect_volume_session(&mut session).map_err(|e| e.to_string())?;
    if !volume.recognized {
        return Err(volume.reason.unwrap_or_else(|| "不是 LTFS 卷".into()));
    }
    let index = volume.index.ok_or_else(|| "没有可用的 index".to_string())?;
    let file = index
        .find_file(path)
        .ok_or_else(|| format!("文件不存在: {path}"))?;

    let mut written = 0u64;
    for extent in &file.extents {
        session
            .locate(extent.partition, extent.start_block)
            .map_err(|e| e.to_string())?;
        let mut remaining = extent.byte_count;
        while remaining > 0 {
            match session.read_record().map_err(|e| e.to_string())? {
                ReadRecord::Data(buf) => {
                    let n = buf.len().min(remaining as usize);
                    out.write_all(&buf[..n])
                        .map_err(|e| format!("写入输出失败: {e}"))?;
                    written += n as u64;
                    remaining -= n as u64;
                }
                _ => return Err(format!("读取 {path} 时磁带记录意外结束")),
            }
        }
    }
    Ok(written)
}

/// 写入结果摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    pub bytes: u64,
    pub files: usize,
    pub directories: usize,
    pub file_uid: u64,
    pub generation: u64,
    pub data_start_block: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePhase {
    Preparing,
    WritingData,
    FinalizingDataIndex,
    SyncingIndexPartition,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteEvent {
    pub phase: WritePhase,
    pub current_file: Option<String>,
    pub files_completed: usize,
    pub files_total: usize,
    pub bytes_written: u64,
    pub bytes_total: u64,
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
        write_with_observer(self.drive, local_path, tape_path, observer)
    }
}

struct WriteProgress<'a> {
    observer: &'a mut dyn FnMut(&WriteEvent),
    files_total: usize,
    bytes_total: u64,
    files_completed: usize,
    bytes_written: u64,
}

impl WriteProgress<'_> {
    fn emit(&mut self, phase: WritePhase, current_file: Option<&str>) {
        (self.observer)(&WriteEvent {
            phase,
            current_file: current_file.map(str::to_owned),
            files_completed: self.files_completed,
            files_total: self.files_total,
            bytes_written: self.bytes_written,
            bytes_total: self.bytes_total,
        });
    }
}

#[derive(Debug)]
struct PlannedFile {
    source: std::path::PathBuf,
    target: String,
    size: u64,
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
            symlink_target: None,
        }));
        plan.total_bytes += metadata.len();
        plan.files.push(PlannedFile {
            source: source.to_path_buf(),
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
    local_path: &Path,
    tape_path: &str,
    observer: &mut dyn FnMut(&WriteEvent),
) -> Result<WriteResult, String> {
    let mut session = TapeSession::open(&drive.sg_path).map_err(|e| e.to_string())?;
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
    let plan = plan_source_tree(&mut index, local_path, tape_path, &now)?;
    let mut progress = WriteProgress {
        observer,
        files_total: plan.files.len(),
        bytes_total: plan.total_bytes,
        files_completed: 0,
        bytes_written: 0,
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

    // 分块写入本地文件内容。普通文件之间不写 filemark；最终由下面写入
    // data index 前的 filemark 结束数据文件区。
    let mut total = 0u64;
    let mut buf = vec![0u8; blocksize];
    let mut first_write = true;
    for planned in &plan.files {
        progress.emit(WritePhase::WritingData, Some(&planned.target));
        let mut file = std::fs::File::open(&planned.source)
            .map_err(|e| format!("打开 {} 失败: {e}", planned.source.display()))?;
        let file_start = data_pos;
        let mut file_total = 0u64;
        loop {
            use std::io::Read;
            let n = file
                .read(&mut buf)
                .map_err(|e| format!("读取 {} 失败: {e}", planned.source.display()))?;
            if n == 0 {
                break;
            }
            if let Err(e) = session.write_record(&buf[..n]) {
                if first_write && is_end_of_data_error(&e) {
                    return Err(format!(
                        "写入被拒绝：驱动器报告的 EOD 与磁带实际内容不一致（刚用 mkltfs 格式化的磁带常见）。\
                         这可能是因为格式化后驱动器 EOD 标记未与内容同步。详情: {e}"
                    ));
                }
                return Err(format!("写入 {} 失败: {e}", planned.source.display()));
            }
            first_write = false;
            file_total += n as u64;
            total += n as u64;
            progress.bytes_written = total;
            data_pos += 1;
        }
        if file_total != planned.size {
            return Err(format!(
                "源文件大小在规划后发生变化：{}（计划 {} 字节，实际读取 {} 字节）",
                planned.source.display(),
                planned.size,
                file_total
            ));
        }
        let entry = index
            .find_file_mut(&planned.target)
            .expect("文件条目已在写入前规划");
        entry.length = file_total;
        entry.extents = if file_total > 0 {
            vec![Extent {
                file_offset: 0,
                partition: label.data_partition,
                start_block: file_start,
                byte_count: file_total,
            }]
        } else {
            Vec::new()
        };
        progress.files_completed += 1;
        progress.emit(WritePhase::WritingData, Some(&planned.target));
    }

    // 更新 index 元数据：generation、时间、UID（位置在下面按分区分别设置）
    index.generation += 1;
    index.highest_file_uid = Some(plan.highest_uid);
    index.update_time = now;

    // 写 data 分区 index：[FM][index][FM]（参考 WriteCurrentIndex）
    progress.emit(WritePhase::FinalizingDataIndex, None);
    session.write_filemark().map_err(|e| e.to_string())?;
    data_pos += 1;
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
    session
        .locate(label.index_partition, index_write_block)
        .map_err(|e| e.to_string())?;
    let mut idx_pos = index_write_block;
    session.write_filemark().map_err(|e| e.to_string())?;
    idx_pos += 1;
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

    progress.emit(WritePhase::Completed, None);

    Ok(WriteResult {
        bytes: total,
        files: plan.files.len(),
        directories: plan.directories,
        file_uid: plan.highest_uid,
        generation: index.generation,
        data_start_block: data_append,
    })
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
                    if let Ok(text) = std::str::from_utf8(&group) {
                        if let Ok(idx) = Index::parse_xml(text) {
                            newer_generation =
                                Some(newer_generation.map_or(idx.generation, |current: u64| {
                                    current.max(idx.generation)
                                }));
                        }
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
fn ltfs_time_now() -> String {
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
