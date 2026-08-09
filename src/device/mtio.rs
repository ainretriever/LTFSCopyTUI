//! Linux st 驱动磁带状态访问（MTIOCGET）。
//!
//! 对应内核 UAPI `linux/mtio.h` 中的 `struct mtget` 与 `MTIOCGET` ioctl。
//! 这是读取当前磁带状态（密度、块大小、位置、generic status）的最直接途径，
//! 不依赖 SCSI 命令。设备层仍以 SCSI（TEST UNIT READY / READ ATTRIBUTE）
//! 作为介质存在性与 MAM 信息的补充来源。

use std::io;
use std::os::unix::io::AsRawFd;

use libc::{c_int, c_long, c_ulong};

/// `_IOR('m', 2, struct mtget)`。
///
/// 按内核 `_IOC` 宏计算：dir=READ(2) 占位 30-31，size 占位 16-29，
/// type('m') 占位 8-15，nr(2) 占位 0-7。
fn mt_ioctl_get() -> c_ulong {
    let size = std::mem::size_of::<MtGet>() as c_ulong;
    (2u64 << 30 | (size as u64) << 16 | ('m' as u64) << 8 | 2) as c_ulong
}

/// 对应内核 `struct mtget`（x86_64 上 48 字节）。
///
/// `__kernel_daddr_t` 在 Linux 上是 32 位 `int`。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct MtGet {
    mt_type: c_long,
    mt_resid: c_long,
    mt_dsreg: c_long,
    mt_gstat: c_long,
    mt_erreg: c_long,
    mt_fileno: c_int,
    mt_blkno: c_int,
}

/// MTIOCGET 返回的磁带状态（已按字段含义解码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapeStatus {
    /// st 驱动的设备类型代码（如 MT_ISSCSI2=0x72）。
    pub drive_type: u32,
    /// 当前密度代码（mt_dsreg 高 8 位）。
    pub density_code: u8,
    /// 当前块大小（mt_dsreg 低 24 位；0 表示可变块）。
    pub block_size: u32,
    /// generic status 原始位（对应 `mt status` 的 General status bits）。
    pub gstat: u32,
    /// 自上次状态读取以来的软错误计数（mt_erreg 低 16 位）。
    pub soft_errors: u32,
    pub file_no: i32,
    pub block_no: i32,
    /// 当前分区号（mt_resid 低 8 位，st 驱动语义）。
    pub partition: u8,
}

// mt_gstat 的 GMT_* 位（linux/mtio.h）。
pub const GMT_EOF: u32 = 0x8000_0000;
pub const GMT_BOT: u32 = 0x4000_0000;
pub const GMT_EOT: u32 = 0x2000_0000;
pub const GMT_SM: u32 = 0x1000_0000; // DDS setmark
pub const GMT_EOD: u32 = 0x0800_0000;
pub const GMT_WR_PROT: u32 = 0x0400_0000;
pub const GMT_ONLINE: u32 = 0x0100_0000;
pub const GMT_D_6250: u32 = 0x0080_0000;
pub const GMT_D_1600: u32 = 0x0040_0000;
pub const GMT_D_800: u32 = 0x0020_0000;
pub const GMT_DR_OPEN: u32 = 0x0004_0000; // door open（无磁带）
pub const GMT_IM_REP_EN: u32 = 0x0001_0000;
pub const GMT_CLN: u32 = 0x0000_8000; // 请求清洁

impl TapeStatus {
    pub fn is_online(&self) -> bool {
        self.gstat & GMT_ONLINE != 0
    }

    pub fn is_bot(&self) -> bool {
        self.gstat & GMT_BOT != 0
    }

    pub fn is_eof(&self) -> bool {
        self.gstat & GMT_EOF != 0
    }

    pub fn is_eot(&self) -> bool {
        self.gstat & GMT_EOT != 0
    }

    pub fn is_eod(&self) -> bool {
        self.gstat & GMT_EOD != 0
    }

    pub fn is_write_protected(&self) -> bool {
        self.gstat & GMT_WR_PROT != 0
    }

    pub fn is_door_open(&self) -> bool {
        self.gstat & GMT_DR_OPEN != 0
    }

    pub fn cleaning_requested(&self) -> bool {
        self.gstat & GMT_CLN != 0
    }
}

/// 对已打开的 /dev/nstX 执行 MTIOCGET。
///
/// 未装载介质时 st 驱动通常返回 EIO/ENXIO，调用方据此结合
/// TEST UNIT READY 判断“无介质”而不是直接视为致命错误。
pub fn get_status(fd: &impl AsRawFd) -> io::Result<TapeStatus> {
    let mut raw = MtGet::default();
    // SAFETY: raw 是与 struct mtget 布局一致的 repr(C) 结构，整个调用期间存活。
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), mt_ioctl_get(), &mut raw) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(TapeStatus {
        drive_type: raw.mt_type as u32,
        density_code: ((raw.mt_dsreg >> 24) & 0xff) as u8,
        block_size: (raw.mt_dsreg & 0xff_ffff) as u32,
        gstat: raw.mt_gstat as u32,
        soft_errors: (raw.mt_erreg & 0xffff) as u32,
        file_no: raw.mt_fileno,
        block_no: raw.mt_blkno,
        partition: (raw.mt_resid & 0xff) as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtget_layout_matches_x86_64_kernel_struct() {
        // struct mtget { long x5; __kernel_daddr_t x2 }，x86_64 上 48 字节。
        assert_eq!(std::mem::size_of::<MtGet>(), 48);
        assert_eq!(std::mem::offset_of!(MtGet, mt_type), 0);
        assert_eq!(std::mem::offset_of!(MtGet, mt_resid), 8);
        assert_eq!(std::mem::offset_of!(MtGet, mt_dsreg), 16);
        assert_eq!(std::mem::offset_of!(MtGet, mt_gstat), 24);
        assert_eq!(std::mem::offset_of!(MtGet, mt_erreg), 32);
        assert_eq!(std::mem::offset_of!(MtGet, mt_fileno), 40);
        assert_eq!(std::mem::offset_of!(MtGet, mt_blkno), 44);
    }

    #[test]
    fn mtioctl_get_number_on_x86_64() {
        // _IOR('m', 2, struct mtget)，x86_64 size=48 → 0x80306d02。
        assert_eq!(mt_ioctl_get(), 0x8030_6d02);
    }

    #[test]
    fn decode_status_bits() {
        let s = TapeStatus {
            drive_type: 0x72,
            density_code: 0x58,
            block_size: 512,
            gstat: GMT_ONLINE | GMT_IM_REP_EN | GMT_WR_PROT,
            soft_errors: 3,
            file_no: 1,
            block_no: 42,
            partition: 0,
        };
        assert!(s.is_online());
        assert!(s.is_write_protected());
        assert!(!s.is_bot());
        assert!(!s.is_door_open());
    }
}
