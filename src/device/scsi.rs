//! 最小 SCSI 命令传输（SG_IO）与命令响应解析。
//!
//! 只依赖 Linux sg 驱动提供的 SG_IO ioctl。当前实现：
//! - INQUIRY（Milestone 0）；
//! - TEST UNIT READY / MODE SENSE / READ ATTRIBUTE（Milestone 1）。
//! 后续的 LOG SENSE、WRITE ATTRIBUTE 等命令可以在此基础上扩展。

use std::io;
use std::os::unix::io::AsRawFd;

use libc::{c_int, c_uint, c_ulong, c_void};

const SG_IO: c_ulong = 0x2285;
const SG_DXFER_FROM_DEV: c_int = -3;
const SG_DXFER_TO_DEV: c_int = -2;
const SG_DXFER_NONE: c_int = -1;
const SG_INTERFACE_ID: c_int = 0x53; // 'S'
const REWIND_OPCODE: u8 = 0x01;
const FORMAT_MEDIUM_OPCODE: u8 = 0x04;
const READ_OPCODE: u8 = 0x08;
const WRITE_OPCODE: u8 = 0x0a;
const WRITE_FILEMARK_OPCODE: u8 = 0x10;
const MODE_SELECT_OPCODE: u8 = 0x15;
const MODE_SELECT10_OPCODE: u8 = 0x55;
const START_STOP_OPCODE: u8 = 0x1b;
const PREVENT_ALLOW_MEDIUM_REMOVAL_OPCODE: u8 = 0x1e;
const INQUIRY_OPCODE: u8 = 0x12;
const TEST_UNIT_READY_OPCODE: u8 = 0x00;
const MODE_SENSE_OPCODE: u8 = 0x1a;
const MODE_SENSE10_OPCODE: u8 = 0x5a;
const READ_ATTRIBUTE_OPCODE: u8 = 0x8c;
const WRITE_ATTRIBUTE_OPCODE: u8 = 0x8d;
const READ_POSITION_OPCODE: u8 = 0x34;
const LOCATE16_OPCODE: u8 = 0x92;
const SPACE16_OPCODE: u8 = 0x91;
const VPD_SERIAL: u8 = 0x80;
const DEFAULT_TIMEOUT_MS: c_uint = 10_000;
/// 会引起磁带运动或把缓存落带的命令可能持续数分钟。
/// LTFSCopyGUI 对 LOCATE 使用 1800 秒；这里对所有数据通道/运动命令采用同一上限。
const TAPE_TIMEOUT_MS: c_uint = 1_800_000;
const SENSE_BUF_LEN: u8 = 32;

#[repr(C)]
#[derive(Clone, Copy)]
struct SgIoHdr {
    interface_id: c_int,
    dxfer_direction: c_int,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: c_uint,
    dxferp: *mut c_void,
    cmdp: *mut u8,
    sbp: *mut u8,
    timeout: c_uint,
    flags: c_uint,
    pack_id: c_int,
    usr_ptr: *mut c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: c_int,
    duration: c_uint,
    info: c_uint,
}

/// SG_IO 执行结果。`status == 0` 表示 SCSI GOOD。
#[derive(Debug, Clone)]
pub struct ScsiResult {
    pub status: u8,
    pub host_status: u16,
    pub driver_status: u16,
    pub resid: c_int,
    /// 发生 CHECK CONDITION 时驱动返回的原始 sense 数据。
    pub sense: Vec<u8>,
}

impl ScsiResult {
    /// SCSI、host 和 driver 三层都成功才算命令成功。
    pub fn is_good(&self) -> bool {
        self.status == 0 && self.host_status == 0 && self.driver_status == 0
    }
}

/// 通过 SG_IO 执行一条 CDB，数据方向为 FROM_DEV（设备 → 主机）。
pub fn sg_io_from_device(fd: &impl AsRawFd, cdb: &[u8], buf: &mut [u8]) -> io::Result<ScsiResult> {
    sg_io_impl(fd, cdb, SG_DXFER_FROM_DEV, buf, DEFAULT_TIMEOUT_MS)
}

/// 通过 SG_IO 执行一条无数据传输的 CDB（如 TEST UNIT READY）。
pub fn sg_io_none(fd: &impl AsRawFd, cdb: &[u8]) -> io::Result<ScsiResult> {
    sg_io_impl(fd, cdb, SG_DXFER_NONE, &mut [], DEFAULT_TIMEOUT_MS)
}

/// 通过 SG_IO 执行一条数据方向为 TO_DEV（主机 → 设备）的 CDB。
pub fn sg_io_to_device(fd: &impl AsRawFd, cdb: &[u8], data: &[u8]) -> io::Result<ScsiResult> {
    sg_io_to_device_timeout(fd, cdb, data, DEFAULT_TIMEOUT_MS)
}

fn sg_io_to_device_timeout(
    fd: &impl AsRawFd,
    cdb: &[u8],
    data: &[u8],
    timeout_ms: c_uint,
) -> io::Result<ScsiResult> {
    if cdb.len() > u8::MAX as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "CDB 过长"));
    }

    let mut sense_buf = [0u8; SENSE_BUF_LEN as usize];
    let mut hdr = SgIoHdr {
        interface_id: SG_INTERFACE_ID,
        dxfer_direction: SG_DXFER_TO_DEV,
        cmd_len: cdb.len() as u8,
        mx_sb_len: SENSE_BUF_LEN,
        iovec_count: 0,
        dxfer_len: data.len() as c_uint,
        dxferp: if data.is_empty() {
            std::ptr::null_mut()
        } else {
            data.as_ptr().cast_mut().cast()
        },
        cmdp: cdb.as_ptr().cast_mut(),
        sbp: sense_buf.as_mut_ptr(),
        timeout: timeout_ms,
        flags: 0,
        pack_id: 0,
        usr_ptr: std::ptr::null_mut(),
        status: 0,
        masked_status: 0,
        msg_status: 0,
        sb_len_wr: 0,
        host_status: 0,
        driver_status: 0,
        resid: 0,
        duration: 0,
        info: 0,
    };

    // SAFETY: 与 sg_io_impl 相同的约束；data 在整个调用期间保持存活。
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), SG_IO, &mut hdr) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }

    let sense = sense_buf[..hdr.sb_len_wr as usize].to_vec();
    Ok(ScsiResult {
        status: hdr.status,
        host_status: hdr.host_status,
        driver_status: hdr.driver_status,
        resid: hdr.resid,
        sense,
    })
}

fn sg_io_impl(
    fd: &impl AsRawFd,
    cdb: &[u8],
    direction: c_int,
    buf: &mut [u8],
    timeout_ms: c_uint,
) -> io::Result<ScsiResult> {
    if cdb.len() > u8::MAX as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "CDB 过长"));
    }

    let mut sense_buf = [0u8; SENSE_BUF_LEN as usize];
    let mut hdr = SgIoHdr {
        interface_id: SG_INTERFACE_ID,
        dxfer_direction: direction,
        cmd_len: cdb.len() as u8,
        mx_sb_len: SENSE_BUF_LEN,
        iovec_count: 0,
        dxfer_len: buf.len() as c_uint,
        dxferp: if buf.is_empty() {
            std::ptr::null_mut()
        } else {
            buf.as_mut_ptr().cast()
        },
        cmdp: cdb.as_ptr().cast_mut(),
        sbp: sense_buf.as_mut_ptr(),
        timeout: timeout_ms,
        flags: 0,
        pack_id: 0,
        usr_ptr: std::ptr::null_mut(),
        status: 0,
        masked_status: 0,
        msg_status: 0,
        sb_len_wr: 0,
        host_status: 0,
        driver_status: 0,
        resid: 0,
        duration: 0,
        info: 0,
    };

    // SAFETY: hdr 是 repr(C) 结构，字段与 Linux sg_io_hdr 一致；
    // buf 与 sense_buf 在整个调用期间保持存活。
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), SG_IO, &mut hdr) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }

    let sense = sense_buf[..hdr.sb_len_wr as usize].to_vec();
    Ok(ScsiResult {
        status: hdr.status,
        host_status: hdr.host_status,
        driver_status: hdr.driver_status,
        resid: hdr.resid,
        sense,
    })
}

/// 发送 SCSI INQUIRY（标准或 EVPD），把响应写入 `buf`。
pub fn inquiry(
    fd: &impl AsRawFd,
    evpd: bool,
    page_code: u8,
    buf: &mut [u8],
) -> io::Result<ScsiResult> {
    let len = buf.len();
    let mut cdb = [0u8; 6];
    cdb[0] = INQUIRY_OPCODE;
    if evpd {
        cdb[1] = 0x01;
    }
    cdb[2] = page_code;
    cdb[3] = (len >> 8) as u8;
    cdb[4] = (len & 0xff) as u8;
    sg_io_from_device(fd, &cdb, buf)
}

/// 发送 SCSI TEST UNIT READY。
///
/// 返回 CHECK CONDITION 时调用方可从 `ScsiResult::sense` 解析具体原因
/// （例如 NOT READY / MEDIUM NOT PRESENT 表示未装载介质）。
pub fn test_unit_ready(fd: &impl AsRawFd) -> io::Result<ScsiResult> {
    let cdb = [TEST_UNIT_READY_OPCODE, 0, 0, 0, 0, 0];
    sg_io_none(fd, &cdb)
}

/// 发送 6 字节 MODE SENSE，读取指定 page（0 表示当前值）。
pub fn mode_sense(fd: &impl AsRawFd, page_code: u8, buf: &mut [u8]) -> io::Result<ScsiResult> {
    let len = buf.len().min(u16::MAX as usize);
    let mut cdb = [0u8; 6];
    cdb[0] = MODE_SENSE_OPCODE;
    cdb[2] = page_code;
    cdb[3] = (len >> 8) as u8;
    cdb[4] = (len & 0xff) as u8;
    sg_io_from_device(fd, &cdb, buf)
}

/// 发送 10 字节 MODE SENSE，支持 subpage（LTFS 写入模式使用 0x10/0x01）。
pub fn mode_sense10(
    fd: &impl AsRawFd,
    page_code: u8,
    subpage_code: u8,
    buf: &mut [u8],
) -> io::Result<ScsiResult> {
    let len = buf.len().min(u16::MAX as usize);
    let mut cdb = [0u8; 10];
    cdb[0] = MODE_SENSE10_OPCODE;
    cdb[2] = page_code & 0x3f; // PC=Current
    cdb[3] = subpage_code;
    cdb[7] = (len >> 8) as u8;
    cdb[8] = len as u8;
    sg_io_from_device(fd, &cdb, buf)
}

/// 发送 16 字节 READ ATTRIBUTE（SSC MAM）。
///
/// `first_attr` 是起始 attribute identifier（0 表示从头读取整个列表）。
pub fn read_attribute(
    fd: &impl AsRawFd,
    first_attr: u16,
    alloc_len: u32,
    buf: &mut [u8],
) -> io::Result<ScsiResult> {
    read_attribute_partition(fd, 0, first_attr, alloc_len, buf)
}

pub fn read_attribute_partition(
    fd: &impl AsRawFd,
    partition: u8,
    first_attr: u16,
    alloc_len: u32,
    buf: &mut [u8],
) -> io::Result<ScsiResult> {
    let mut cdb = [0u8; 16];
    cdb[0] = READ_ATTRIBUTE_OPCODE;
    cdb[7] = partition;
    cdb[8] = (first_attr >> 8) as u8;
    cdb[9] = (first_attr & 0xff) as u8;
    cdb[10] = (alloc_len >> 24) as u8;
    cdb[11] = (alloc_len >> 16) as u8;
    cdb[12] = (alloc_len >> 8) as u8;
    cdb[13] = (alloc_len & 0xff) as u8;
    sg_io_from_device(fd, &cdb, buf)
}

/// 发送 WRITE ATTRIBUTE，把一个或多个 MAM attribute descriptor 写入分区。
pub fn write_attribute(
    fd: &impl AsRawFd,
    partition: u8,
    descriptors: &[u8],
) -> io::Result<ScsiResult> {
    let total_len = descriptors.len().saturating_add(4);
    if total_len > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MAM attribute 过长",
        ));
    }
    let mut parameter = Vec::with_capacity(total_len);
    parameter.extend_from_slice(&(total_len as u32).to_be_bytes());
    parameter.extend_from_slice(descriptors);
    let mut cdb = [0u8; 16];
    cdb[0] = WRITE_ATTRIBUTE_OPCODE;
    cdb[1] = 0x01; // write through
    cdb[7] = partition;
    cdb[10..14].copy_from_slice(&(total_len as u32).to_be_bytes());
    sg_io_to_device_timeout(fd, &cdb, &parameter, TAPE_TIMEOUT_MS)
}

/// 发送 SCSI REWIND。
pub fn rewind(fd: &impl AsRawFd) -> io::Result<ScsiResult> {
    let cdb = [REWIND_OPCODE, 0, 0, 0, 0, 0];
    sg_io_impl(fd, &cdb, SG_DXFER_NONE, &mut [], TAPE_TIMEOUT_MS)
}

/// 发送 SSC FORMAT MEDIUM；format=1 创建分区介质。
pub fn format_medium(fd: &impl AsRawFd, format: u8) -> io::Result<ScsiResult> {
    let cdb = [FORMAT_MEDIUM_OPCODE, 0, format, 0, 0, 0];
    sg_io_impl(fd, &cdb, SG_DXFER_NONE, &mut [], TAPE_TIMEOUT_MS)
}

/// 发送 SCSI START STOP UNIT（用于 load / unload 磁带）。
///
/// `start=true` 启动介质（装载）；`loej=true` 同时弹出（unload/eject）。
pub fn start_stop_unit(
    fd: &impl AsRawFd,
    start: bool,
    loej: bool,
    immed: bool,
) -> io::Result<ScsiResult> {
    let mut cdb = [0u8; 6];
    cdb[0] = START_STOP_OPCODE;
    if immed {
        cdb[1] = 0x01;
    }
    if loej {
        cdb[4] |= 0x02;
    }
    if start {
        cdb[4] |= 0x01;
    }
    sg_io_impl(fd, &cdb, SG_DXFER_NONE, &mut [], TAPE_TIMEOUT_MS)
}

/// 发送 SCSI PREVENT ALLOW MEDIUM REMOVAL。
///
/// `prevent=true` 锁定介质（防止移除），`false` 解锁。
pub fn prevent_allow_medium_removal(fd: &impl AsRawFd, prevent: bool) -> io::Result<ScsiResult> {
    let mut cdb = [0u8; 6];
    cdb[0] = PREVENT_ALLOW_MEDIUM_REMOVAL_OPCODE;
    if prevent {
        cdb[4] = 0x01;
    }
    sg_io_none(fd, &cdb)
}

/// 发送 SCSI READ(6)。可变块模式下 transfer length 以字节为单位，
/// 每次返回一条记录（不足时按实际长度返回）。
pub fn read6(fd: &impl AsRawFd, buf: &mut [u8]) -> io::Result<ScsiResult> {
    let len = buf.len().min(0xff_ffff);
    let mut cdb = [0u8; 6];
    cdb[0] = READ_OPCODE;
    cdb[2] = (len >> 16) as u8;
    cdb[3] = (len >> 8) as u8;
    cdb[4] = (len & 0xff) as u8;
    sg_io_impl(fd, &cdb, SG_DXFER_FROM_DEV, buf, TAPE_TIMEOUT_MS)
}

/// 发送 SCSI WRITE(6)。可变块模式下 transfer length 以字节为单位，
/// 每次写入一条记录。
pub fn write6(fd: &impl AsRawFd, data: &[u8]) -> io::Result<ScsiResult> {
    let len = data.len().min(0xff_ffff);
    let mut cdb = [0u8; 6];
    cdb[0] = WRITE_OPCODE;
    cdb[2] = (len >> 16) as u8;
    cdb[3] = (len >> 8) as u8;
    cdb[4] = (len & 0xff) as u8;
    sg_io_to_device_timeout(fd, &cdb, data, TAPE_TIMEOUT_MS)
}

/// 发送 SCSI WRITE FILEMARK(6)。
pub fn write_filemark(fd: &impl AsRawFd, count: u8) -> io::Result<ScsiResult> {
    let cdb = [WRITE_FILEMARK_OPCODE, 0, 0, 0, count, 0];
    sg_io_impl(fd, &cdb, SG_DXFER_NONE, &mut [], TAPE_TIMEOUT_MS)
}

/// 发送 SCSI READ POSITION（long format）。
pub fn read_position(fd: &impl AsRawFd, buf: &mut [u8]) -> io::Result<ScsiResult> {
    let mut cdb = [0u8; 10];
    cdb[0] = READ_POSITION_OPCODE;
    cdb[1] = 0x06; // Long format
    sg_io_from_device(fd, &cdb, buf)
}

/// 发送 SCSI LOCATE(16)。
///
/// `change_partition` 为 true 时设置 CP 位，允许跨分区定位。
pub fn locate16(
    fd: &impl AsRawFd,
    partition: u8,
    block: u64,
    change_partition: bool,
) -> io::Result<ScsiResult> {
    let mut cdb = [0u8; 16];
    cdb[0] = LOCATE16_OPCODE;
    if change_partition {
        cdb[1] = 0x02;
    }
    cdb[3] = partition;
    let bytes = block.to_be_bytes();
    cdb[4..12].copy_from_slice(&bytes);
    sg_io_impl(fd, &cdb, SG_DXFER_NONE, &mut [], TAPE_TIMEOUT_MS)
}

/// 发送 SCSI SPACE(16)。
///
/// `code`: 0=块，1=filemark，3=end of data；`count` 为有符号数，负值表示向后。
pub fn space16(fd: &impl AsRawFd, code: u8, count: i64) -> io::Result<ScsiResult> {
    let mut cdb = [0u8; 16];
    cdb[0] = SPACE16_OPCODE;
    cdb[1] = code;
    cdb[4..12].copy_from_slice(&count.to_be_bytes());
    sg_io_impl(fd, &cdb, SG_DXFER_NONE, &mut [], TAPE_TIMEOUT_MS)
}

/// SPACE 到 end of data（快速定位分区 EOD，避免逐块读到 blank check）。
pub fn space_to_eod(fd: &impl AsRawFd) -> io::Result<ScsiResult> {
    space16(fd, 0x03, 0)
}

/// 发送 6 字节 MODE SELECT（PF=1），用于设置块大小等。
pub fn mode_select6(fd: &impl AsRawFd, data: &[u8]) -> io::Result<ScsiResult> {
    let len = data.len().min(u16::MAX as usize);
    let mut cdb = [0u8; 6];
    cdb[0] = MODE_SELECT_OPCODE;
    cdb[1] = 0x10; // PF
    cdb[4] = len as u8;
    sg_io_to_device_timeout(fd, &cdb, data, TAPE_TIMEOUT_MS)
}

/// 发送 10 字节 MODE SELECT（PF=1）。
pub fn mode_select10(fd: &impl AsRawFd, data: &[u8]) -> io::Result<ScsiResult> {
    let len = data.len().min(u16::MAX as usize);
    let mut cdb = [0u8; 10];
    cdb[0] = MODE_SELECT10_OPCODE;
    cdb[1] = 0x10; // PF
    cdb[7] = (len >> 8) as u8;
    cdb[8] = len as u8;
    sg_io_to_device_timeout(fd, &cdb, data, TAPE_TIMEOUT_MS)
}

/// READ POSITION 响应解析（long format）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapePosition {
    pub partition: u32,
    pub block: u64,
    pub filemarks: u64,
}

pub fn parse_read_position(buf: &[u8]) -> Option<TapePosition> {
    if buf.len() < 24 {
        return None;
    }
    Some(TapePosition {
        partition: u32::from_be_bytes(buf[4..8].try_into().ok()?),
        block: u64::from_be_bytes(buf[8..16].try_into().ok()?),
        filemarks: u64::from_be_bytes(buf[16..24].try_into().ok()?),
    })
}

/// 从 6 字节 MODE SENSE 响应中取第一个块描述符的密度代码。
///
/// 响应前 4 字节是 mode parameter header，之后是块描述符，
/// 磁带块描述符第 0 字节为密度代码（LTFSCopyGUI 同样按偏移 4 读取）。
pub fn parse_mode_sense_density(buf: &[u8]) -> Option<u8> {
    buf.get(4).copied()
}

/// 固定格式 sense data 的关键字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenseData {
    pub key: u8,
    pub asc: u8,
    pub ascq: u8,
}

/// 解析固定/描述符格式 sense data。无法识别时返回 `None`。
pub fn parse_sense(sense: &[u8]) -> Option<SenseData> {
    match sense.first().copied()? & 0x7f {
        0x70 | 0x71 => Some(SenseData {
            key: sense.get(2).copied()? & 0x0f,
            asc: sense.get(12).copied()?,
            ascq: sense.get(13).copied()?,
        }),
        0x72 | 0x73 => Some(SenseData {
            key: sense.get(2).copied()? & 0x0f,
            asc: sense.get(3).copied()?,
            ascq: sense.get(4).copied()?,
        }),
        _ => None,
    }
}

/// 一个 MAM attribute 描述符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MamAttribute<'a> {
    pub id: u16,
    /// 低 2 位为格式：0=binary，1=ascii，2=text。
    pub format: u8,
    pub value: &'a [u8],
}

/// 解析 READ ATTRIBUTE 响应中的 attribute 列表。
///
/// 响应前 4 字节是头部（列表长度），之后是若干描述符：
/// `id(2) | format(1) | length(2) | value(length)`。
pub fn parse_mam_attributes(buf: &[u8]) -> Vec<MamAttribute<'_>> {
    let mut out = Vec::new();
    let mut off = 4usize;
    while off + 5 <= buf.len() {
        let id = u16::from_be_bytes([buf[off], buf[off + 1]]);
        let format = buf[off + 2] & 0x03;
        let len = u16::from_be_bytes([buf[off + 3], buf[off + 4]]) as usize;
        let start = off + 5;
        let end = (start + len).min(buf.len());
        out.push(MamAttribute {
            id,
            format,
            value: &buf[start..end],
        });
        if end == buf.len() {
            break;
        }
        off = end;
    }
    out
}

/// 把 MAM ASCII/text 值转为字符串：去掉尾部空白与 NUL。
pub fn ascii_value(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect::<String>()
        .trim()
        .to_string()
}

/// 把 MAM binary 值按大端解析为 u64（1/2/4/8 字节）。
pub fn u64_value(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    Some(bytes.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64))
}

/// 解析标准 INQUIRY 响应中的 Vendor / Product / Revision。
pub fn parse_standard_inquiry(buf: &[u8]) -> (String, String, String) {
    let vendor = ascii_field(buf.get(8..16).unwrap_or_default());
    let model = ascii_field(buf.get(16..32).unwrap_or_default());
    let revision = ascii_field(buf.get(32..36).unwrap_or_default());
    (vendor, model, revision)
}

/// 解析 VPD 0x80（Unit Serial Number）。
pub fn parse_serial(buf: &[u8]) -> String {
    if buf.len() < 4 || buf.get(1).copied() != Some(VPD_SERIAL) {
        return String::new();
    }
    let len = ((buf[2] as usize) << 8) | buf[3] as usize;
    let end = (4 + len).min(buf.len());
    ascii_field(&buf[4..end])
}

/// 把固定长度 ASCII 字段转为字符串：截断到 NUL，并去掉尾部空白。
fn ascii_field(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(status: u8, host_status: u16, driver_status: u16) -> ScsiResult {
        ScsiResult {
            status,
            host_status,
            driver_status,
            resid: 0,
            sense: Vec::new(),
        }
    }

    #[test]
    fn good_requires_scsi_host_and_driver_success() {
        assert!(result(0, 0, 0).is_good());
        assert!(!result(2, 0, 0).is_good());
        assert!(!result(0, 7, 0).is_good());
        assert!(!result(0, 0, 8).is_good());
    }
    use std::mem;

    #[test]
    fn parse_standard_inquiry_response() {
        let mut buf = [b' '; 96];
        buf[0] = 0x00;
        buf[1] = 0x01;
        buf[8..16].copy_from_slice(b"QUANTUM ");
        buf[16..32].copy_from_slice(b"ULTRIUM 5       ");
        buf[32..36].copy_from_slice(b"3210");
        let (vendor, model, revision) = parse_standard_inquiry(&buf);
        assert_eq!(vendor, "QUANTUM");
        assert_eq!(model, "ULTRIUM 5");
        assert_eq!(revision, "3210");
    }

    #[test]
    fn parse_serial_vpd() {
        let mut buf = vec![0u8; 256];
        buf[0] = 0x00;
        buf[1] = 0x80;
        buf[2] = 0x00;
        buf[3] = 10;
        buf[4..14].copy_from_slice(b"HU1340YHGE");
        assert_eq!(parse_serial(&buf), "HU1340YHGE");
    }

    #[test]
    fn parse_serial_trims_trailing_spaces() {
        let mut buf = vec![0u8; 32];
        buf[1] = 0x80;
        buf[2] = 0x00;
        buf[3] = 8;
        buf[4..12].copy_from_slice(b"AB12    ");
        assert_eq!(parse_serial(&buf), "AB12");
    }

    #[test]
    fn parse_wrong_page_returns_empty() {
        let mut buf = vec![0u8; 32];
        buf[1] = 0x83;
        assert_eq!(parse_serial(&buf), "");
    }

    #[test]
    fn parse_inquiry_handles_nul_padding() {
        let mut buf = [0u8; 96];
        buf[8..12].copy_from_slice(b"TEST");
        let (vendor, model, _) = parse_standard_inquiry(&buf);
        assert_eq!(vendor, "TEST");
        assert_eq!(model, "");
    }

    #[test]
    fn mem_layout_of_sg_io_hdr() {
        // 对照 /usr/include/scsi/sg.h 的关键字段偏移，防止结构漂移。
        assert_eq!(mem::offset_of!(SgIoHdr, interface_id), 0);
        assert_eq!(mem::offset_of!(SgIoHdr, dxfer_direction), 4);
        assert_eq!(mem::offset_of!(SgIoHdr, cmd_len), 8);
        assert_eq!(mem::offset_of!(SgIoHdr, status), 64);
        assert_eq!(mem::offset_of!(SgIoHdr, resid), 72);
        assert_eq!(mem::size_of::<SgIoHdr>(), 88);
    }

    #[test]
    fn mode_sense_density_offset() {
        // 12 字节响应：header(4) + 块描述符(8)，密度在偏移 4。
        let mut buf = vec![0u8; 12];
        buf[3] = 8; // block descriptor length
        buf[4] = 0x58; // LTO-5 density
        assert_eq!(parse_mode_sense_density(&buf), Some(0x58));
        assert_eq!(parse_mode_sense_density(&[]), None);
    }

    #[test]
    fn parse_fixed_sense() {
        // fixed format: NOT READY / MEDIUM NOT PRESENT (3A 00)
        let mut sense = [0u8; 18];
        sense[0] = 0x70;
        sense[2] = 0x02;
        sense[7] = 0x0a;
        sense[12] = 0x3a;
        sense[13] = 0x00;
        let s = parse_sense(&sense).unwrap();
        assert_eq!((s.key, s.asc, s.ascq), (0x02, 0x3a, 0x00));
    }

    #[test]
    fn parse_descriptor_sense() {
        let mut sense = [0u8; 8];
        sense[0] = 0x72;
        sense[2] = 0x02;
        sense[3] = 0x04;
        sense[4] = 0x01;
        let s = parse_sense(&sense).unwrap();
        assert_eq!((s.key, s.asc, s.ascq), (0x02, 0x04, 0x01));
    }

    #[test]
    fn parse_mam_attribute_list() {
        // 头部 4 字节 + 3 个描述符（对应真实磁带响应格式）。
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x2d]);
        // 0x0000 剩余容量，binary，8 字节
        buf.extend_from_slice(&[0x00, 0x00, 0x80, 0x00, 0x08]);
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x16, 0x3f, 0x77]);
        // 0x0806 barcode，ascii，8 字节
        buf.extend_from_slice(&[0x08, 0x06, 0x81, 0x00, 0x08]);
        buf.extend_from_slice(b"E6008A  ");
        // 0x0401 介质序列号，ascii，8 字节
        buf.extend_from_slice(&[0x04, 0x01, 0x81, 0x00, 0x08]);
        buf.extend_from_slice(b"MF9EVDEA");

        let attrs = parse_mam_attributes(&buf);
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0].id, 0x0000);
        assert_eq!(u64_value(attrs[0].value), Some(0x163f77));
        assert_eq!(attrs[1].id, 0x0806);
        assert_eq!(ascii_value(attrs[1].value), "E6008A");
        assert_eq!(attrs[2].id, 0x0401);
        assert_eq!(ascii_value(attrs[2].value), "MF9EVDEA");
    }

    #[test]
    fn parse_mam_truncated_value_is_clamped() {
        let mut buf = vec![0u8; 4];
        // 声明 32 字节但只有 2 字节可用
        buf.extend_from_slice(&[0x08, 0x06, 0x81, 0x00, 0x20, b'E', b'6']);
        let attrs = parse_mam_attributes(&buf);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].value, b"E6");
    }

    #[test]
    fn u64_value_sizes() {
        assert_eq!(u64_value(&[0x01]), Some(1));
        assert_eq!(
            u64_value(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x16, 0x3f, 0x77]),
            Some(0x163f77)
        );
        assert_eq!(u64_value(&[]), None);
        assert_eq!(u64_value(&[0; 9]), None);
    }

    #[test]
    fn parse_read_position_long_format() {
        let mut buf = vec![0u8; 32];
        buf[4..8].copy_from_slice(&1u32.to_be_bytes());
        buf[8..16].copy_from_slice(&42u64.to_be_bytes());
        buf[16..24].copy_from_slice(&3u64.to_be_bytes());
        let pos = parse_read_position(&buf).unwrap();
        assert_eq!(pos.partition, 1);
        assert_eq!(pos.block, 42);
        assert_eq!(pos.filemarks, 3);
        assert_eq!(parse_read_position(&buf[..20]), None);
    }
}
