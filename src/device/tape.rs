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
        if !result.is_good() {
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
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SELECT"));
        }
        Ok(())
    }

    /// 确保驱动器处于 LTFS 所需的 write-anywhere 模式。
    ///
    /// SSC device configuration extension mode page (0x10/0x01) 的高半字节
    /// 表示 append-only 模式。OpenLTFS 在关闭该模式时会先卸载介质、执行
    /// MODE SELECT，再重新装载；Quantum LTO-5 也要求这个顺序。
    pub fn ensure_write_anywhere(&mut self) -> Result<(), Error> {
        const PAGE_LEN: usize = 48;
        let mut page = [0u8; PAGE_LEN];
        let result = scsi::mode_sense10(&self.fd, 0x10, 0x01, &mut page).map_err(|e| {
            Error::Io {
                context: format!("向 {} 发送 MODE SENSE 0x10/0x01 失败", self.path.display()),
                source: e,
            }
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SENSE 0x10/0x01"));
        }
        if page[21] & 0xf0 == 0 {
            return Ok(());
        }

        let result = scsi::start_stop_unit(&self.fd, false, false, false).map_err(|e| {
            Error::Io {
                context: format!("为切换 write-anywhere 卸载 {} 失败", self.path.display()),
                source: e,
            }
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "UNLOAD for write-anywhere"));
        }
        self.partition = None;

        // MODE SELECT parameter header 的长度字段必须清零；清除 mode page 的
        // PS 位，并把 append-only 字段设为 0。偏移与 OpenLTFS 的 48-byte
        // TC_MP_DEV_CONFIG_EXT 页面处理一致。
        page[0] = 0;
        page[1] = 0;
        page[16] &= 0x7f;
        page[21] &= 0x0f;
        let result = scsi::mode_select10(&self.fd, &page).map_err(|e| Error::Io {
            context: format!("向 {} 发送 write-anywhere MODE SELECT 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SELECT write-anywhere"));
        }

        let result = scsi::start_stop_unit(&self.fd, true, false, false).map_err(|e| {
            Error::Io {
                context: format!("切换 write-anywhere 后重新装载 {} 失败", self.path.display()),
                source: e,
            }
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "LOAD after write-anywhere"));
        }
        self.set_variable_block()
    }

    /// SCSI LOAD：装载/穿带当前介质（对已装载磁带是无害的幂等操作）。
    pub fn load(&mut self) -> Result<(), Error> {
        let result = scsi::start_stop_unit(&self.fd, true, false, false).map_err(|e| Error::Io {
            context: format!("向 {} 发送 LOAD 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "LOAD"));
        }
        Ok(())
    }

    /// SCSI UNLOAD：弹出介质（eject）。
    ///
    /// 与 st 驱动 MTOFFL 语义一致：先解除介质移除保护（门锁），
    /// 再发送 START STOP（全 0，驱动器据此执行卸载/弹出）。
    pub fn unload(&mut self) -> Result<(), Error> {
        let result = scsi::prevent_allow_medium_removal(&self.fd, false).map_err(|e| Error::Io {
            context: format!("向 {} 发送 PREVENT ALLOW 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "PREVENT ALLOW MEDIUM REMOVAL"));
        }
        // st 驱动 MTOFFL 先回绕；START STOP 全 0 触发驱动器卸载/弹出。
        self.rewind()?;
        let result = scsi::start_stop_unit(&self.fd, false, false, false).map_err(|e| {
            Error::Io {
                context: format!("向 {} 发送 UNLOAD 失败", self.path.display()),
                source: e,
            }
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "UNLOAD"));
        }
        self.partition = None;
        Ok(())
    }

    /// SCSI REWIND（当前分区 BOT）。
    pub fn rewind(&mut self) -> Result<(), Error> {
        let result = scsi::rewind(&self.fd).map_err(|e| Error::Io {
            context: format!("向 {} 发送 REWIND 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
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
        if !result.is_good() {
            return Err(self.scsi_error(&result, "READ POSITION"));
        }
        let pos = scsi::parse_read_position(&buf).ok_or_else(|| Error::Io {
            context: format!("{} READ POSITION 响应过短", self.path.display()),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, "响应不足 24 字节"),
        })?;
        self.partition = Some(pos.partition);
        Ok(pos)
    }

    /// SPACE 到当前分区 EOD，并返回 EOD 块号。
    pub fn space_to_eod(&mut self) -> Result<u64, Error> {
        let result = scsi::space_to_eod(&self.fd).map_err(|e| Error::Io {
            context: format!("向 {} 发送 SPACE(EOD) 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "SPACE(EOD)"));
        }
        Ok(self.read_position()?.block)
    }

    /// 定位到指定分区的指定块；跨分区时自动设置 LOCATE 的 CP 位。
    pub fn locate(&mut self, partition: u8, block: u64) -> Result<(), Error> {
        let change = self.partition != Some(partition as u32);
        let result = scsi::locate16(&self.fd, partition, block, change).map_err(|e| Error::Io {
            context: format!("向 {} 发送 LOCATE 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
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
        if result.is_good() {
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

    /// 写入一条记录（可变块模式，长度任意）。
    pub fn write_record(&mut self, data: &[u8]) -> Result<(), Error> {
        let result = scsi::write6(&self.fd, data).map_err(|e| Error::Io {
            context: format!("向 {} 发送 WRITE 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "WRITE"));
        }
        Ok(())
    }

    /// 写入一个 filemark。
    pub fn write_filemark(&mut self) -> Result<(), Error> {
        let result = scsi::write_filemark(&self.fd, 1).map_err(|e| Error::Io {
            context: format!("向 {} 发送 WRITE FILEMARK 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "WRITE FILEMARK"));
        }
        Ok(())
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
