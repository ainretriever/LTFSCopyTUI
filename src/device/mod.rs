//! 磁带设备层：发现 Linux 磁带机并读取设备身份。
//!
//! 本层不知道 LTFS、TUI 或任何业务工作流。它只负责：
//! - 通过 sysfs 枚举 /dev/nstX，并匹配对应的 /dev/sgX；
//! - 通过 SG_IO 发送 SCSI INQUIRY，取得 Vendor/Model/Revision/Serial。

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
