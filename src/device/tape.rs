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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MamAttributeFormat {
    Binary = 0,
    Ascii = 1,
    Text = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MamAttributeRecord {
    pub id: u16,
    pub format: u8,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongEraseStatus {
    InProgress { progress: Option<u16> },
    Complete,
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
        options
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK);
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
        let result =
            scsi::mode_sense10(&self.fd, 0x10, 0x01, &mut page).map_err(|e| Error::Io {
                context: format!("向 {} 发送 MODE SENSE 0x10/0x01 失败", self.path.display()),
                source: e,
            })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SENSE 0x10/0x01"));
        }
        if page[21] & 0xf0 == 0 {
            return Ok(());
        }

        let result =
            scsi::start_stop_unit(&self.fd, false, false, false).map_err(|e| Error::Io {
                context: format!("为切换 write-anywhere 卸载 {} 失败", self.path.display()),
                source: e,
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
            context: format!(
                "向 {} 发送 write-anywhere MODE SELECT 失败",
                self.path.display()
            ),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SELECT write-anywhere"));
        }

        let result =
            scsi::start_stop_unit(&self.fd, true, false, false).map_err(|e| Error::Io {
                context: format!(
                    "切换 write-anywhere 后重新装载 {} 失败",
                    self.path.display()
                ),
                source: e,
            })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "LOAD after write-anywhere"));
        }
        self.set_variable_block()
    }

    /// 按 LTFS 标准布局把介质初始化为两个分区：P0 为最小 index 分区，
    /// P1 使用其余容量作为 data 分区。
    pub fn format_ltfs_partitions(&mut self) -> Result<(), Error> {
        self.load()?;
        let mut page = [0u8; 28];
        let result = scsi::mode_sense10(&self.fd, 0x11, 0, &mut page).map_err(|e| Error::Io {
            context: format!("向 {} 读取 medium partition page 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SENSE medium partition"));
        }
        configure_ltfs_partition_page(&mut page)?;
        let page_len = (18usize + page[17] as usize).max(28).min(page.len());
        let result = scsi::mode_select10(&self.fd, &page[..page_len]).map_err(|e| Error::Io {
            context: format!("向 {} 设置 LTFS 分区参数失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SELECT medium partition"));
        }
        let result = scsi::format_medium(&self.fd, 0x01).map_err(|e| Error::Io {
            context: format!("向 {} 发送 FORMAT MEDIUM 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "FORMAT MEDIUM partition"));
        }
        self.partition = None;
        Ok(())
    }

    /// 快速擦除当前介质。ERASE(6) LONG=0 会使原有逻辑内容不可访问，
    /// 但不会对整盘介质执行物理重写。
    pub fn short_erase(&mut self) -> Result<(), Error> {
        self.load()?;
        self.rewind()?;
        let result = scsi::erase(&self.fd, false, false).map_err(|e| Error::Io {
            context: format!("向 {} 发送 short ERASE 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "ERASE short"));
        }
        self.partition = None;
        Ok(())
    }

    /// 在当前位置启动异步 long erase。使用 IMMED 后由 `long_erase_status`
    /// 轮询，因此全带擦除不会被单条 SG_IO 的 1800 秒 timeout 截断。
    pub fn start_long_erase(&mut self) -> Result<(), Error> {
        self.rewind()?;
        let result = scsi::erase(&self.fd, true, true).map_err(|e| Error::Io {
            context: format!("向 {} 启动 long ERASE 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "ERASE long IMMED"));
        }
        Ok(())
    }

    pub fn long_erase_status(&mut self) -> Result<LongEraseStatus, Error> {
        let mut sense = [0u8; 32];
        let result = scsi::request_sense(&self.fd, &mut sense).map_err(|e| Error::Io {
            context: format!("向 {} 查询 long ERASE 状态失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "REQUEST SENSE during long ERASE"));
        }
        classify_long_erase_sense(&sense)
    }

    /// 建立 LTFSCopyGUI 使用的最小 P0 + 剩余 P1 临时分区。
    pub fn create_minimum_erase_partition(&mut self) -> Result<(), Error> {
        self.load()?;
        let mut page = [0u8; 28];
        let result = scsi::mode_sense10(&self.fd, 0x11, 0, &mut page).map_err(|e| Error::Io {
            context: format!(
                "向 {} 读取 minimum-erase partition page 失败",
                self.path.display()
            ),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SENSE minimum-erase partition"));
        }
        configure_ltfs_partition_page(&mut page)?;
        let page_len = (18usize + page[17] as usize).max(28).min(page.len());
        let result = scsi::mode_select10(&self.fd, &page[..page_len]).map_err(|e| Error::Io {
            context: format!(
                "向 {} 设置 minimum-erase partition 失败",
                self.path.display()
            ),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "MODE SELECT minimum-erase partition"));
        }
        let result = scsi::format_medium(&self.fd, 0x01).map_err(|e| Error::Io {
            context: format!(
                "向 {} 创建 minimum-erase partition 失败",
                self.path.display()
            ),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "FORMAT MEDIUM minimum-erase partition"));
        }
        self.partition = None;
        Ok(())
    }

    /// 按 LTFSCopyGUI 的 0x0a/0x01 序列重新穿带，使驱动器重新识别刚修改的
    /// 分区布局，同时不执行普通 CLI unload 的门锁/弹出流程。
    pub fn rethread(&mut self) -> Result<(), Error> {
        for (action, name) in [(0x0a, "UNTHREAD"), (0x01, "THREAD")] {
            let result = scsi::load_unload_action(&self.fd, action).map_err(|e| Error::Io {
                context: format!("向 {} 发送 {name} 失败", self.path.display()),
                source: e,
            })?;
            if !result.is_good() {
                return Err(self.scsi_error(&result, name));
            }
        }
        self.partition = None;
        Ok(())
    }

    /// FORMAT MEDIUM type 0 移除临时分区，恢复未分区介质。
    pub fn remove_partitions(&mut self) -> Result<(), Error> {
        let result = scsi::format_medium(&self.fd, 0x00).map_err(|e| Error::Io {
            context: format!("向 {} 移除临时分区失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "FORMAT MEDIUM remove partitions"));
        }
        self.partition = None;
        Ok(())
    }

    pub fn write_mam_attribute(
        &mut self,
        partition: u8,
        id: u16,
        format: MamAttributeFormat,
        value: &[u8],
    ) -> Result<(), Error> {
        if value.len() > u16::MAX as usize {
            return Err(Error::Protocol("MAM attribute value 过长".into()));
        }
        let mut descriptor = Vec::with_capacity(value.len() + 5);
        descriptor.extend_from_slice(&id.to_be_bytes());
        descriptor.push(format as u8);
        descriptor.extend_from_slice(&(value.len() as u16).to_be_bytes());
        descriptor.extend_from_slice(value);
        let result =
            scsi::write_attribute(&self.fd, partition, &descriptor).map_err(|e| Error::Io {
                context: format!(
                    "向 {} 写 MAM attribute 0x{id:04x} 失败",
                    self.path.display()
                ),
                source: e,
            })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, &format!("WRITE ATTRIBUTE 0x{id:04x}")));
        }
        Ok(())
    }

    pub fn read_mam_attribute(&mut self, partition: u8, id: u16) -> Result<Vec<u8>, Error> {
        let mut buf = vec![0u8; 512];
        let result =
            scsi::read_attribute_partition(&self.fd, partition, id, buf.len() as u32, &mut buf)
                .map_err(|e| Error::Io {
                    context: format!(
                        "从 {} 读取 MAM attribute 0x{id:04x} 失败",
                        self.path.display()
                    ),
                    source: e,
                })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, &format!("READ ATTRIBUTE 0x{id:04x}")));
        }
        let len = buf.len().saturating_sub(result.resid.max(0) as usize);
        let attribute = scsi::parse_mam_attributes(&buf[..len])
            .into_iter()
            .find(|attribute| attribute.id == id)
            .ok_or_else(|| Error::Protocol(format!("MAM attribute 0x{id:04x} 不存在")))?;
        Ok(attribute.value.to_vec())
    }

    /// 读取一个分区可见的完整 MAM attribute 列表，供诊断和一致性检查使用。
    pub fn read_mam_attributes(&mut self, partition: u8) -> Result<Vec<MamAttributeRecord>, Error> {
        const ALLOC_LEN: usize = 8192;
        let mut buf = vec![0u8; ALLOC_LEN];
        let result =
            scsi::read_attribute_partition(&self.fd, partition, 0, ALLOC_LEN as u32, &mut buf)
                .map_err(|e| Error::Io {
                    context: format!(
                        "从 {} 读取 partition {partition} MAM 列表失败",
                        self.path.display()
                    ),
                    source: e,
                })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, &format!("READ ATTRIBUTE partition {partition}")));
        }
        let len = buf.len().saturating_sub(result.resid.max(0) as usize);
        Ok(scsi::parse_mam_attributes(&buf[..len])
            .into_iter()
            .map(|attribute| MamAttributeRecord {
                id: attribute.id,
                format: attribute.format,
                value: attribute.value.to_vec(),
            })
            .collect())
    }

    /// SCSI LOAD：装载/穿带当前介质（对已装载磁带是无害的幂等操作）。
    pub fn load(&mut self) -> Result<(), Error> {
        let result =
            scsi::start_stop_unit(&self.fd, true, false, false).map_err(|e| Error::Io {
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
        let result =
            scsi::prevent_allow_medium_removal(&self.fd, false).map_err(|e| Error::Io {
                context: format!("向 {} 发送 PREVENT ALLOW 失败", self.path.display()),
                source: e,
            })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "PREVENT ALLOW MEDIUM REMOVAL"));
        }
        // st 驱动 MTOFFL 先回绕；START STOP 全 0 触发驱动器卸载/弹出。
        self.rewind()?;
        let result =
            scsi::start_stop_unit(&self.fd, false, false, false).map_err(|e| Error::Io {
                context: format!("向 {} 发送 UNLOAD 失败", self.path.display()),
                source: e,
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

    /// 用 WRITE FILEMARK count=0 强制驱动器把缓存落带。
    pub fn flush(&mut self) -> Result<(), Error> {
        let result = scsi::write_filemark(&self.fd, 0).map_err(|e| Error::Io {
            context: format!("向 {} 发送 FLUSH 失败", self.path.display()),
            source: e,
        })?;
        if !result.is_good() {
            return Err(self.scsi_error(&result, "WRITE FILEMARK(0) / FLUSH"));
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

fn configure_ltfs_partition_page(page: &mut [u8]) -> Result<(), Error> {
    if page.len() < 28 || page[16] & 0x3f != 0x11 || page[18] == 0 {
        return Err(Error::Protocol(
            "驱动器不支持创建 LTFS 所需的额外分区".into(),
        ));
    }
    page[0] = 0;
    page[1] = 0;
    page[16] &= 0x7f; // 清除 PS
    page[19] = 1; // one additional partition
    page[20] = 0x20 | (page[20] & 0x1f); // IDP=1, FDP=SDP=0
    page[22] = 0x09; // partition size unit: GB / minimum-unit compatible
    page[24] = 0x00;
    page[25] = 0x01; // P0: minimum partition
    page[26] = 0xff;
    page[27] = 0xff; // P1: remaining capacity
    Ok(())
}

fn classify_long_erase_sense(sense: &[u8]) -> Result<LongEraseStatus, Error> {
    let Some(parsed) = scsi::parse_sense(sense) else {
        return Ok(LongEraseStatus::Complete);
    };
    // Quantum LTO-5 返回 NOT READY(0x02)/00/18；OpenLTFS 的判断只比较
    // ASC/ASCQ 00/16、00/18。两种 sense key 表示法都作为正常进度处理。
    if matches!(parsed.key, 0x00 | 0x02) && parsed.asc == 0 && matches!(parsed.ascq, 0x16 | 0x18) {
        let progress = (sense.len() >= 18).then(|| u16::from_be_bytes([sense[16], sense[17]]));
        return Ok(LongEraseStatus::InProgress { progress });
    }
    if parsed.key == 0 && parsed.asc == 0 && parsed.ascq == 0 {
        return Ok(LongEraseStatus::Complete);
    }
    Err(Error::Protocol(format!(
        "long erase 返回异常 sense: key=0x{:02x} asc=0x{:02x} ascq=0x{:02x}",
        parsed.key, parsed.asc, parsed.ascq
    )))
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn configures_openltfs_compatible_partition_page() {
        let mut page = [0u8; 28];
        page[16] = 0x91;
        page[17] = 0x0a;
        page[18] = 1;
        configure_ltfs_partition_page(&mut page).unwrap();
        assert_eq!(page[16], 0x11);
        assert_eq!(page[19], 1);
        assert_eq!(page[20] & 0xe0, 0x20);
        assert_eq!(&page[24..28], &[0, 1, 0xff, 0xff]);
    }

    #[test]
    fn rejects_drive_without_extra_partition_support() {
        let mut page = [0u8; 28];
        page[16] = 0x11;
        assert!(configure_ltfs_partition_page(&mut page).is_err());
    }

    #[test]
    fn classifies_long_erase_progress_and_completion() {
        let mut active = [0u8; 32];
        active[0] = 0x70;
        active[2] = 0x02;
        active[12] = 0x00;
        active[13] = 0x18;
        active[16] = 0x12;
        active[17] = 0x34;
        assert_eq!(
            classify_long_erase_sense(&active).unwrap(),
            LongEraseStatus::InProgress {
                progress: Some(0x1234)
            }
        );

        let complete = [0u8; 32];
        assert_eq!(
            classify_long_erase_sense(&complete).unwrap(),
            LongEraseStatus::Complete
        );
    }
}
