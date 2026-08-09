//! SCSI 密度代码 → 格式名称映射。
//!
//! 密度代码本身是 T10/SCSI 规范中的事实数据。本表以 mt-st 源码
//! （https://github.com/iustin/mt-st，GPL-2.0）中的 `density_tbl` 为参考整理，
//! 覆盖当前 tapecpy 目标平台（LTO）以及常见的 DLT/SDLT/3592/DDS 格式。
//!
//! 未知代码由调用方显示为十六进制原文，不应视为解析失败。

/// 返回密度代码对应的格式名称；未知代码返回 `None`。
pub fn density_name(code: u8) -> Option<&'static str> {
    DENSITY_TABLE
        .binary_search_by_key(&code, |(c, _)| *c)
        .ok()
        .map(|i| DENSITY_TABLE[i].1)
}

/// LTO 密度代码对应的标准 8 位条码介质代际码（Lx / M8）。
///
/// LTO 物理标签规范：8 位大写字母数字 = 6 位卷序列 + 2 位介质代际码。
/// 常规数据磁带为 L1-L9；LTO-7 M8 磁带为 M8；WORM 磁带使用
/// LT/LU/LV 等变体（不在此枚举，需要时由用户输入）。
pub fn lto_generation_suffix(code: u8) -> Option<&'static str> {
    match code {
        0x40 => Some("L1"),
        0x42 => Some("L2"),
        0x44 => Some("L3"),
        0x46 => Some("L4"),
        0x58 => Some("L5"),
        0x5a => Some("L6"),
        0x5c => Some("L7"),
        0x5d => Some("M8"),
        0x5e => Some("L8"),
        0x60 => Some("L9"),
        _ => None,
    }
}

/// (密度代码, 格式名称)。按代码升序排列，便于二分查找。
const DENSITY_TABLE: &[(u8, &str)] = &[
    (0x00, "default"),
    (0x01, "NRZI (800 bpi) 9-track reel"),
    (0x02, "PE (1600 bpi) 9-track reel"),
    (0x03, "GCR (6250 bpi) 9-track reel"),
    (0x04, "QIC-11"),
    (0x05, "QIC-45/60 (GCR, 8000 bpi)"),
    (0x06, "PE (3200 bpi) 9-track reel"),
    (0x07, "IMFM (6400 bpi)"),
    (0x08, "GCR (8000 bpi)"),
    (0x09, "3480/3490E, GCR (37871 bpi)"),
    (0x0a, "MFM (6667 bpi)"),
    (0x0b, "PE (1600 bpi)"),
    (0x0c, "GCR (12960 bpi)"),
    (0x0d, "GCR (25380 bpi)"),
    (0x0f, "QIC-120 (GCR 10000 bpi)"),
    (0x10, "QIC-150/250 (GCR 10000 bpi)"),
    (0x11, "QIC-320/525 (GCR 16000 bpi)"),
    (0x12, "QIC-1350 (RLL 51667 bpi)"),
    (0x13, "DDS (61000 bpi)"),
    (0x14, "EXB-8200 (RLL 43245 bpi)"),
    (0x15, "EXB-8500 or QIC-1000"),
    (0x16, "MFM 10000 bpi"),
    (0x17, "MFM 42500 bpi"),
    (0x18, "TZ86"),
    (0x19, "DLT 10GB"),
    (0x1a, "DLT 20GB"),
    (0x1b, "DLT 35GB"),
    (0x1c, "QIC-385M"),
    (0x1d, "QIC-410M"),
    (0x1e, "QIC-1000C"),
    (0x1f, "QIC-2100C"),
    (0x20, "QIC-6GB"),
    (0x21, "QIC-20GB"),
    (0x22, "QIC-2GB"),
    (0x23, "QIC-875"),
    (0x24, "DDS-2"),
    (0x25, "DDS-3"),
    (0x26, "DDS-4 or QIC-4GB"),
    (0x27, "Exabyte Mammoth"),
    (0x28, "Exabyte Mammoth-2"),
    (0x29, "QIC-3080MC, IBM 3590 B"),
    (0x2a, "IBM 3590 E"),
    (0x30, "AIT-1 or MLR3"),
    (0x31, "AIT-2"),
    (0x32, "AIT-3 or SLR7"),
    (0x33, "SLR6"),
    (0x34, "SLR100"),
    (0x40, "LTO-1 (Ultrium 1)"),
    (0x41, "LTO-2 (Ultrium 2)"),
    (0x42, "LTO-2"),
    (0x44, "LTO-3"),
    (0x45, "QIC-3095-MC (TR-4)"),
    (0x46, "LTO-4"),
    (0x47, "DDS-5 or TR-5"),
    (0x48, "SDLT220"),
    (0x49, "SDLT320"),
    (0x4a, "SDLT600, T10000A"),
    (0x4b, "T10000B"),
    (0x4c, "T10000C"),
    (0x4d, "T10000D"),
    (0x51, "IBM 3592 J1A"),
    (0x52, "IBM 3592 E05 (TS1120)"),
    (0x53, "IBM 3592 E06 (TS1130)"),
    (0x54, "IBM 3592 E07 (TS1140)"),
    (0x55, "IBM 3592 E08 (TS1150)"),
    (0x56, "IBM 3592 55F (TS1155)"),
    (0x57, "IBM 3592 60F (TS1160)"),
    (0x58, "LTO-5"),
    (0x59, "IBM 3592 70F (TS1170)"),
    (0x5a, "LTO-6"),
    (0x5c, "LTO-7"),
    (0x5d, "LTO-7 M8"),
    (0x5e, "LTO-8"),
    (0x60, "LTO-9"),
    (0x71, "IBM 3592 J1A, encrypted"),
    (0x72, "IBM 3592 E05, encrypted"),
    (0x73, "IBM 3592 E06, encrypted"),
    (0x74, "IBM 3592 E07, encrypted"),
    (0x75, "IBM 3592 E08, encrypted"),
    (0x76, "IBM 3592 55F, encrypted"),
    (0x77, "IBM 3592 60F, encrypted"),
    (0x79, "IBM 3592 70F, encrypted"),
    (0x80, "DLT 15GB uncomp. or Ecrix"),
    (0x81, "DLT 15GB compressed"),
    (0x82, "DLT 20GB uncompressed"),
    (0x83, "DLT 20GB compressed"),
    (0x84, "DLT 35GB uncompressed"),
    (0x85, "DLT 35GB compressed"),
    (0x86, "DLT1 40 GB uncompressed"),
    (0x87, "DLT1 40 GB compressed"),
    (0x88, "DLT 40GB uncompressed"),
    (0x89, "DLT 40GB compressed"),
    (0x8c, "EXB-8505 compressed"),
    (0x90, "SDLT110 uncompr/EXB-8205 compr"),
    (0x91, "SDLT110 compressed"),
    (0x92, "SDLT160 uncompressed"),
    (0x93, "SDLT160 compressed"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_lto_codes() {
        assert_eq!(density_name(0x58), Some("LTO-5"));
        assert_eq!(density_name(0x5a), Some("LTO-6"));
        assert_eq!(density_name(0x5c), Some("LTO-7"));
        assert_eq!(density_name(0x5e), Some("LTO-8"));
        assert_eq!(density_name(0x60), Some("LTO-9"));
    }

    #[test]
    fn unknown_code_returns_none() {
        assert_eq!(density_name(0xfe), None);
        assert_eq!(density_name(0x00), Some("default"));
    }

    #[test]
    fn lto_generation_suffix_mapping() {
        assert_eq!(lto_generation_suffix(0x58), Some("L5"));
        assert_eq!(lto_generation_suffix(0x5a), Some("L6"));
        assert_eq!(lto_generation_suffix(0x5c), Some("L7"));
        assert_eq!(lto_generation_suffix(0x5d), Some("M8"));
        assert_eq!(lto_generation_suffix(0x60), Some("L9"));
        assert_eq!(lto_generation_suffix(0x19), None);
    }

    #[test]
    fn table_is_sorted_by_code() {
        let codes: Vec<u8> = DENSITY_TABLE.iter().map(|(c, _)| *c).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted);
        assert!(codes.windows(2).all(|w| w[0] != w[1]), "代码不允许重复");
    }
}
