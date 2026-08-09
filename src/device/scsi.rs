//! 最小 SCSI 命令传输（SG_IO）与 INQUIRY 解析。
//!
//! 只依赖 Linux sg 驱动提供的 SG_IO ioctl。后续的 LOG SENSE、
//! MODE SENSE、READ ATTRIBUTE 等命令可以在此基础上扩展。

use std::io;
use std::os::unix::io::AsRawFd;

use libc::{c_int, c_uint, c_ulong, c_void};

const SG_IO: c_ulong = 0x2285;
const SG_DXFER_FROM_DEV: c_int = -3;
const SG_INTERFACE_ID: c_int = 0x53; // 'S'
const INQUIRY_OPCODE: u8 = 0x12;
const VPD_SERIAL: u8 = 0x80;
const DEFAULT_TIMEOUT_MS: c_uint = 10_000;
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

/// 通过 SG_IO 执行一条 CDB，数据方向为 FROM_DEV（设备 → 主机）。
pub fn sg_io_from_device(fd: &impl AsRawFd, cdb: &[u8], buf: &mut [u8]) -> io::Result<ScsiResult> {
    if cdb.len() > u8::MAX as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "CDB 过长"));
    }

    let mut sense_buf = [0u8; SENSE_BUF_LEN as usize];
    let mut hdr = SgIoHdr {
        interface_id: SG_INTERFACE_ID,
        dxfer_direction: SG_DXFER_FROM_DEV,
        cmd_len: cdb.len() as u8,
        mx_sb_len: SENSE_BUF_LEN,
        iovec_count: 0,
        dxfer_len: buf.len() as c_uint,
        dxferp: buf.as_mut_ptr().cast(),
        cmdp: cdb.as_ptr().cast_mut(),
        sbp: sense_buf.as_mut_ptr(),
        timeout: DEFAULT_TIMEOUT_MS,
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
}
