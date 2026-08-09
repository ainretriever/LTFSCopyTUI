//! 磁带设备层：发现 Linux 磁带机并读取设备身份。
//!
//! 本层不知道 LTFS、TUI 或任何业务工作流。它只负责：
//! - 通过 sysfs 枚举 /dev/nstX，并匹配对应的 /dev/sgX；
//! - 通过 SG_IO 发送 SCSI 命令，取得设备身份与介质信息；
//! - 通过 st 驱动的 MTIOCGET 读取磁带状态。

pub mod density;
pub mod mtio;
pub mod scsi;
pub mod sysfs;

use std::fmt;
use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapeDrive {
    /// 无回绕设备名，如 "nst0"。
    pub name: String,
    /// /dev/nstX（无回绕）。
    pub nst_path: PathBuf,
    /// /dev/stX（关闭时自动回绕）。
    pub st_path: PathBuf,
    /// /dev/sgX（SCSI generic）。
    pub sg_path: PathBuf,
    pub vendor: String,
    pub model: String,
    pub revision: String,
    pub serial: String,
}

/// 设备访问错误。
///
/// 保留底层细节（io 错误、SCSI status 与原始 sense 数据），
/// 以便上层构造透明诊断信息。
#[derive(Debug)]
pub enum Error {
    Io {
        context: String,
        source: std::io::Error,
    },
    Scsi {
        device: String,
        status: u8,
        host_status: u16,
        driver_status: u16,
        sense: Vec<u8>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { context, source } => write!(f, "{context}: {source}"),
            Error::Scsi {
                device,
                status,
                host_status,
                driver_status,
                sense,
            } => {
                write!(
                    f,
                    "{device}: SCSI status=0x{status:02x} host=0x{host_status:04x} driver=0x{driver_status:04x}",
                )?;
                if !sense.is_empty() {
                    let hex: Vec<String> = sense.iter().map(|b| format!("{b:02x}")).collect();
                    write!(f, " sense=[{}]", hex.join(" "))?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Scsi { .. } => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            context: "sysfs 访问失败".into(),
            source,
        }
    }
}

/// 介质装载状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaPresence {
    /// 已装载介质。
    Loaded,
    /// 驱动器明确报告无介质。
    NotLoaded,
    /// 驱动器未就绪（例如正在加载/初始化），介质存在与否暂不确定。
    NotReady,
    /// 无法取得明确状态。
    #[default]
    Unknown,
}

/// 从 MAM（READ ATTRIBUTE）读取的介质信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MamInfo {
    pub barcode: Option<String>,
    pub volume_identifier: Option<String>,
    pub medium_manufacturer: Option<String>,
    pub medium_serial: Option<String>,
    pub medium_type: Option<u8>,
    pub remaining_capacity_mib: Option<u64>,
    pub max_capacity_mib: Option<u64>,
    pub load_count: Option<u64>,
    pub tape_alert_flags: Option<u64>,
    pub total_written_mib: Option<u64>,
    pub total_read_mib: Option<u64>,
}

/// Milestone 1 介质检查结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaInfo {
    pub presence: MediaPresence,
    /// MTIOCGET 状态（含密度、位置、generic status）；无介质时可能为 None。
    pub tape_status: Option<mtio::TapeStatus>,
    /// 解析出的密度代码（优先 MTIOCGET，其次 MODE SENSE）。
    pub density_code: Option<u8>,
    /// MAM 介质信息；只有介质已装载且驱动器支持时才存在。
    pub mam: Option<MamInfo>,
}

impl MediaInfo {
    /// 密度代码对应的格式名称（来自密度表）。
    pub fn density_name(&self) -> Option<&'static str> {
        self.density_code.and_then(density::density_name)
    }

    /// 当 MAM barcode 只有 6 位卷序列而介质是 LTO 时，
    /// 推导标准 8 位物理标签（6 位卷序列 + 介质代际码，如 E6008A + L5）。
    ///
    /// 仅供显示核对物理标签，不修改 MAM 内容，也不视为权威数据。
    pub fn full_label_hint(&self) -> Option<String> {
        let barcode = self.mam.as_ref()?.barcode.as_deref()?;
        if barcode.len() != 6 {
            return None;
        }
        let suffix = density::lto_generation_suffix(self.density_code?)?;
        Some(format!("{barcode}{suffix}"))
    }
}

/// 检查一台磁带机的介质状态与基本信息（Milestone 1）。
///
/// 读取操作全部使用 O_NONBLOCK，不改变磁带位置或设备状态：
/// 1. MTIOCGET —— st 驱动状态（密度、块大小、位置、generic status）；
/// 2. TEST UNIT READY —— 判定介质是否装载；
/// 3. MODE SENSE —— 密度代码补充来源；
/// 4. READ ATTRIBUTE（MAM）—— barcode、容量、介质序列号等。
pub fn inspect_media(drive: &TapeDrive) -> Result<MediaInfo, Error> {
    let mut info = MediaInfo::default();

    let nst = open_tape_node(&drive.nst_path, false)?;
    match mtio::get_status(&nst) {
        Ok(status) => {
            info.density_code = Some(status.density_code);
            info.tape_status = Some(status);
        }
        // st 驱动在无介质时可能直接返回 EIO/ENXIO，交给 TUR 判定。
        Err(e) if matches!(e.raw_os_error(), Some(libc::EIO) | Some(libc::ENXIO)) => {}
        Err(e) => {
            return Err(Error::Io {
                context: format!("读取 {} 状态失败", drive.nst_path.display()),
                source: e,
            })
        }
    }

    // READ ATTRIBUTE（MAM）需要 sg 设备以读写方式打开；本函数只发送
    // 非破坏性命令（TEST UNIT READY / MODE SENSE / READ ATTRIBUTE）。
    let sg = open_tape_node(&drive.sg_path, true)?;
    info.presence = classify_unit_ready(&sg, &drive.sg_path)?;

    if info.density_code.is_none() {
        info.density_code = mode_sense_density(&sg, &drive.sg_path)?;
    }

    if info.presence == MediaPresence::Loaded {
        info.mam = Some(read_mam(&sg, &drive.sg_path)?);
    }

    Ok(info)
}

/// 以 O_NONBLOCK 打开磁带/SCSI 节点。
///
/// 磁带状态查询（MTIOCGET）只读即可；SCSI 的某些命令（如 READ ATTRIBUTE）
/// 需要 sg 设备以读写方式打开。
fn open_tape_node(path: &std::path::Path, writable: bool) -> Result<std::fs::File, Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(writable).custom_flags(libc::O_NONBLOCK);
    options.open(path).map_err(|e| Error::Io {
        context: format!("打开 {} 失败", path.display()),
        source: e,
    })
}

/// 用 TEST UNIT READY 判定介质存在性。
fn classify_unit_ready(sg: &std::fs::File, path: &std::path::Path) -> Result<MediaPresence, Error> {
    let result = scsi::test_unit_ready(sg).map_err(|e| Error::Io {
        context: format!("向 {} 发送 TEST UNIT READY 失败", path.display()),
        source: e,
    })?;
    match classify_tur_presence(result.status, &result.sense) {
        MediaPresence::Unknown => Err(Error::Scsi {
            device: path.display().to_string(),
            status: result.status,
            host_status: result.host_status,
            driver_status: result.driver_status,
            sense: result.sense,
        }),
        p => Ok(p),
    }
}

/// 根据 TEST UNIT READY 的 SCSI status 与 sense data 判定介质存在性。
///
/// 独立成纯函数便于单元测试“无介质”路径。
fn classify_tur_presence(status: u8, sense: &[u8]) -> MediaPresence {
    if status == 0 {
        return MediaPresence::Loaded;
    }
    match scsi::parse_sense(sense) {
        Some(s) if s.key == 0x02 && s.asc == 0x3a => MediaPresence::NotLoaded,
        Some(s) if s.key == 0x02 => MediaPresence::NotReady,
        _ => MediaPresence::Unknown,
    }
}

/// 用 MODE SENSE 读取密度代码（作为 MTIOCGET 的补充来源）。
fn mode_sense_density(sg: &std::fs::File, path: &std::path::Path) -> Result<Option<u8>, Error> {
    let mut buf = [0u8; 12];
    let result = scsi::mode_sense(sg, 0, &mut buf).map_err(|e| Error::Io {
        context: format!("向 {} 发送 MODE SENSE 失败", path.display()),
        source: e,
    })?;
    if result.status != 0 {
        return Err(Error::Scsi {
            device: path.display().to_string(),
            status: result.status,
            host_status: result.host_status,
            driver_status: result.driver_status,
            sense: result.sense,
        });
    }
    Ok(scsi::parse_mode_sense_density(&buf))
}

/// 读取并解析 MAM attribute 列表，提取 Milestone 1 关心的字段。
fn read_mam(sg: &std::fs::File, path: &std::path::Path) -> Result<MamInfo, Error> {
    const MAM_ALLOC_LEN: usize = 8192;
    let mut buf = vec![0u8; MAM_ALLOC_LEN];
    let result = scsi::read_attribute(sg, 0, MAM_ALLOC_LEN as u32, &mut buf).map_err(|e| {
        Error::Io {
            context: format!("向 {} 发送 READ ATTRIBUTE 失败", path.display()),
            source: e,
        }
    })?;
    if result.status != 0 {
        return Err(Error::Scsi {
            device: path.display().to_string(),
            status: result.status,
            host_status: result.host_status,
            driver_status: result.driver_status,
            sense: result.sense,
        });
    }
    let len = buf.len().saturating_sub(result.resid.max(0) as usize);
    let mut mam = MamInfo::default();
    for attr in scsi::parse_mam_attributes(&buf[..len]) {
        match attr.id {
            0x0000 => mam.remaining_capacity_mib = scsi::u64_value(attr.value),
            0x0001 => mam.max_capacity_mib = scsi::u64_value(attr.value),
            0x0002 => mam.tape_alert_flags = scsi::u64_value(attr.value),
            0x0003 => mam.load_count = scsi::u64_value(attr.value),
            0x0008 => mam.volume_identifier = Some(scsi::ascii_value(attr.value)),
            0x0220 => mam.total_written_mib = scsi::u64_value(attr.value),
            0x0221 => mam.total_read_mib = scsi::u64_value(attr.value),
            0x0400 => mam.medium_manufacturer = Some(scsi::ascii_value(attr.value)),
            0x0401 => mam.medium_serial = Some(scsi::ascii_value(attr.value)),
            0x0408 => mam.medium_type = attr.value.first().copied(),
            0x0806 => mam.barcode = Some(scsi::ascii_value(attr.value)),
            _ => {}
        }
    }
    Ok(mam)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_sense(key: u8, asc: u8, ascq: u8) -> Vec<u8> {
        let mut sense = vec![0u8; 18];
        sense[0] = 0x70;
        sense[2] = key;
        sense[7] = 0x0a;
        sense[12] = asc;
        sense[13] = ascq;
        sense
    }

    #[test]
    fn tur_good_means_loaded() {
        assert_eq!(classify_tur_presence(0, &[]), MediaPresence::Loaded);
    }

    #[test]
    fn tur_medium_not_present_means_not_loaded() {
        let sense = fixed_sense(0x02, 0x3a, 0x00);
        assert_eq!(classify_tur_presence(0x02, &sense), MediaPresence::NotLoaded);
        // 3A/02：介质不在位，但 MAM 可访问
        let sense = fixed_sense(0x02, 0x3a, 0x02);
        assert_eq!(classify_tur_presence(0x02, &sense), MediaPresence::NotLoaded);
    }

    #[test]
    fn tur_not_ready_other_asc_means_not_ready() {
        // 正在加载/初始化：NOT READY / 逻辑单元未就绪
        let sense = fixed_sense(0x02, 0x04, 0x01);
        assert_eq!(classify_tur_presence(0x02, &sense), MediaPresence::NotReady);
    }

    #[test]
    fn tur_unrecognized_sense_means_unknown() {
        let sense = fixed_sense(0x05, 0x20, 0x00); // ILLEGAL REQUEST
        assert_eq!(classify_tur_presence(0x02, &sense), MediaPresence::Unknown);
        assert_eq!(classify_tur_presence(0x02, &[]), MediaPresence::Unknown);
    }
}

/// 发现系统上全部磁带机（sysfs 根固定为 /sys）。
pub fn discover() -> Result<Vec<TapeDrive>, Error> {
    discover_in(Path::new("/sys"))
}

/// 从给定的 sysfs 根目录发现磁带机（便于测试）。
pub fn discover_in(sysfs_root: &Path) -> Result<Vec<TapeDrive>, Error> {
    let mut drives = Vec::new();
    for nst_name in sysfs::enumerate_tape_bases(sysfs_root)? {
        let st_name = format!("st{}", &nst_name[3..]);
        let sg_name = match sysfs::find_sg_for_tape(sysfs_root, &nst_name) {
            Some(name) => name,
            // 没有对应 sg 节点时跳过，不视为整体失败。
            None => continue,
        };
        let sg_path = PathBuf::from(format!("/dev/{sg_name}"));
        let drive = read_drive_identity(&nst_name, &st_name, &sg_path)?;
        drives.push(drive);
    }
    Ok(drives)
}

/// 打开 /dev/sgX 并读取 INQUIRY 身份信息。
fn read_drive_identity(
    nst_name: &str,
    st_name: &str,
    sg_path: &Path,
) -> Result<TapeDrive, Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(false).custom_flags(libc::O_NONBLOCK);
    let file = options.open(sg_path).map_err(|e| Error::Io {
        context: format!("打开 {} 失败", sg_path.display()),
        source: e,
    })?;

    let mut inquiry_buf = [0u8; 96];
    let n = send_inquiry(&file, sg_path, false, 0, &mut inquiry_buf)?;
    let (vendor, model, revision) = scsi::parse_standard_inquiry(&inquiry_buf[..n]);

    let mut serial_buf = [0u8; 256];
    let n = send_inquiry(&file, sg_path, true, 0x80, &mut serial_buf)?;
    let serial = scsi::parse_serial(&serial_buf[..n]);

    Ok(TapeDrive {
        name: nst_name.to_string(),
        nst_path: PathBuf::from(format!("/dev/{nst_name}")),
        st_path: PathBuf::from(format!("/dev/{st_name}")),
        sg_path: sg_path.to_path_buf(),
        vendor,
        model,
        revision,
        serial,
    })
}

fn send_inquiry(
    file: &File,
    sg_path: &Path,
    evpd: bool,
    page_code: u8,
    buf: &mut [u8],
) -> Result<usize, Error> {
    let result = scsi::inquiry(file, evpd, page_code, buf).map_err(|e| Error::Io {
        context: format!("向 {} 发送 INQUIRY 失败", sg_path.display()),
        source: e,
    })?;
    if result.status != 0 {
        return Err(Error::Scsi {
            device: sg_path.display().to_string(),
            status: result.status,
            host_status: result.host_status,
            driver_status: result.driver_status,
            sense: result.sense,
        });
    }
    Ok(buf.len().saturating_sub(result.resid as usize))
}
