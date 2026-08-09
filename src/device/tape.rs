//! 面向 LTFS 读取的磁带会话（基于 /dev/sgX 的 SCSI 原语）。
//!
//! LTFS 以可变块模式读写磁带：每条记录（record）对应一次 SCSI READ/WRITE。
//! 本模块提供定位、读位置、读记录等原语，并维护会话内已知的当前分区，
//! 以便在需要跨分区定位时正确设置 LOCATE 的 CP 位。
//!
//! 本层不知道 LTFS label/index 的结构，只处理字节记录与 FileMark/EOD。

use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::device::{Error, scsi};

/// 单条记录读取上限。LTO 驱动器最大块大小为 1 MiB。
pub const READ_BUF_LEN: usize = 1024 * 1024;

/// 读取一条磁带记录的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadRecord {
    Data(Vec<u8>),
    Filemark,
    Eod,
}

/// 一次面向单台磁带机的只读会话。
pub struct TapeSession {
    fd: File,
    path: PathBuf,
    /// 最近一次 READ POSITION 确认的分区；用于 LOCATE 的 CP 判断。
    partition: Option<u32>,
}

impl TapeSession {
    /// 打开 /dev/sgX（O_RDWR | O_NONBLOCK）。
    pub fn open(path: &Path) -> Result<TapeSession, Error> {
        let mut options = File::options();
        options.read(true).write(true).custom_flags(libc::O_NONBLOCK);
        let fd = options.open(path).map_err(|e| Error::Io {
            context: format!("打开 {} 失败", path.display()),
            source: e,
        })?;
        Ok(TapeSession {
            fd,
            path: path.to_path_buf(),
            partition: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 把驱动器设置为可变块模式（块描述符 block length = 0）。
    ///
    /// 需要保留当前密度代码，因此先 MODE SENSE 再 MODE SELECT。
    pub fn set_variable_block(&mut self) -> Result<(), Error> {
        let mut sense_buf = [0u8; 12];
        let result = scsi::mode_sense(&self.fd, 0, &mut sense_buf).map_err(|e| Error::Io {
            context: format!("向 {} 发送 MODE SENSE 失败", self.path.display()),
            source: e,
        })?;
        if result.status != 0 {
            return Err(self.scsi_error(&result, "MODE SENSE"));
        }
        let density = scsi::parse_mode_sense_density(&sense_buf).unwrap_or(0);

        // mode parameter header(4) + 块描述符(8)：密度 + block length=0（可变块）
        let mut data = [0u8; 12];
        data[2] = 0x10; // device-specific parameter（DPF）
        data[3] = 8; // block descriptor length
        data[4] = density;
        let result = scsi::mode_select6(&self.fd, &data).map_err(|e| Error::Io {
            context: format!("向 {} 发送 MODE SELECT 失败", self.path.display()),
            source: e,
        })?;
        if result.status != 0 {
            return Err(self.scsi_error(&result, "MODE SELECT"));
        }
        Ok(())
    }

    /// SCSI REWIND（当前分区 BOT）。
    pub fn rewind(&mut self) -> Result<(), Error> {
        let result = scsi::rewind(&self.fd).map_err(|e| Error::Io {
            context: format!("向 {} 发送 REWIND 失败", self.path.display()),
            source: e,
        })?;
        if result.status != 0 {
            return Err(self.scsi_error(&result, "REWIND"));
        }
        self.partition = Some(self.read_position()?.partition);
        Ok(())
    }

    /// 读取当前分区与块位置（long format）。
    pub fn read_position(&mut self) -> Result<scsi::TapePosition, Error> {
        let mut buf = [0u8; 32];
        let result = scsi::read_position(&self.fd, &mut buf).map_err(|e| Error::Io {
            context: format!("向 {} 发送 READ POSITION 失败", self.path.display()),
            source: e,
        })?;
        if result.status != 0 {
            return Err(self.scsi_error(&result, "READ POSITION"));
        }
        let pos = scsi::parse_read_position(&buf).ok_or_else(|| Error::Io {
            context: format!("{} READ POSITION 响应过短", self.path.display()),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, "响应不足 24 字节"),
        })?;
        self.partition = Some(pos.partition);
        Ok(pos)
    }

    /// 定位到指定分区的指定块；跨分区时自动设置 LOCATE 的 CP 位。
    pub fn locate(&mut self, partition: u8, block: u64) -> Result<(), Error> {
        let change = self.partition != Some(partition as u32);
        let result = scsi::locate16(&self.fd, partition, block, change).map_err(|e| Error::Io {
            context: format!("向 {} 发送 LOCATE 失败", self.path.display()),
            source: e,
        })?;
        if result.status != 0 {
            return Err(self.scsi_error(&result, "LOCATE"));
        }
        // 定位后向设备确认实际位置（OpenLTFS 同样在 LOCATE 后读取位置）。
        let pos = self.read_position()?;
        self.partition = Some(pos.partition);
        Ok(())
    }

    /// 读取一条记录（可变块模式）。
    ///
    /// 记录大于缓冲区时返回错误；遇到 FileMark 返回 `Filemark`；
    /// 读到 EOD（blank check）返回 `Eod`。
    pub fn read_record(&mut self) -> Result<ReadRecord, Error> {
        let mut buf = vec![0u8; READ_BUF_LEN];
        let result = scsi::read6(&self.fd, &mut buf).map_err(|e| Error::Io {
            context: format!("向 {} 发送 READ 失败", self.path.display()),
            source: e,
        })?;
        if result.status == 0 {
            let len = READ_BUF_LEN.saturating_sub(result.resid.max(0) as usize);
            buf.truncate(len);
            return Ok(ReadRecord::Data(buf));
        }

        // CHECK CONDITION：区分 FileMark / 短读(ILI) / EOD
        if let Some(s) = scsi::parse_sense(&result.sense) {
            let flags = result.sense.get(2).copied().unwrap_or(0);
            if flags & 0x80 != 0 {
                return Ok(ReadRecord::Filemark);
            }
            if s.key == 0x08 {
                // BLANK CHECK：已到数据末尾
                return Ok(ReadRecord::Eod);
            }
            if flags & 0x20 != 0 {
                // ILI：记录小于请求长度，resid 给出差值
                let len = READ_BUF_LEN.saturating_sub(result.resid.max(0) as usize);
                if len == 0 {
                    return Err(self.scsi_error(&result, "READ（记录超过缓冲区）"));
                }
                buf.truncate(len);
                return Ok(ReadRecord::Data(buf));
            }
        }
        Err(self.scsi_error(&result, "READ"))
    }

    fn scsi_error(&self, result: &scsi::ScsiResult, op: &str) -> Error {
        Error::Scsi {
            device: format!("{} ({op})", self.path.display()),
            status: result.status,
            host_status: result.host_status,
            driver_status: result.driver_status,
            sense: result.sense.clone(),
        }
    }
}
