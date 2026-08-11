//! LTO 厂商 diagnostic 87h/88h 通道错误率页面。
//!
//! 算法与 LTFSCopyGUI `ReadChanLRInfo` 保持一致：
//! `log10(delta C1 / delta CCP / 2 / 1920)`。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    Read,
    Write,
}

impl PageKind {
    pub fn page_code(self) -> u8 {
        match self {
            Self::Read => 0x87,
            Self::Write => 0x88,
        }
    }

    fn summary_fields(self) -> usize {
        match self {
            Self::Read => 5,
            Self::Write => 4,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelCounters {
    pub c1_errors: u64,
    pub c1_uncorrectable: u64,
    pub header_errors: u64,
    pub write_pass_errors: u64,
    pub ccps: u64,
}

pub fn parse_page(data: &[u8], kind: PageKind) -> Result<Vec<ChannelCounters>, String> {
    if data.len() < 4 {
        return Err("diagnostic page header 截断".into());
    }
    if data[0] != kind.page_code() {
        return Err(format!(
            "diagnostic page code 不匹配：期望 0x{:02x}，实际 0x{:02x}",
            kind.page_code(),
            data[0]
        ));
    }
    let declared = u16::from_be_bytes([data[2], data[3]]) as usize;
    if data.len() < declared + 4 {
        return Err("diagnostic page 正文截断".into());
    }
    let text = std::str::from_utf8(&data[4..4 + declared])
        .map_err(|_| "diagnostic page 不是 ASCII/UTF-8".to_string())?;
    let fields: Vec<&str> = text.split_ascii_whitespace().collect();
    let channel_fields = fields
        .get(kind.summary_fields()..)
        .ok_or_else(|| "diagnostic page 缺少 summary 字段".to_string())?;
    if channel_fields.len() % 5 != 0 {
        return Err(format!(
            "diagnostic channel 字段数 {} 不是 5 的倍数",
            channel_fields.len()
        ));
    }
    channel_fields
        .chunks_exact(5)
        .map(|fields| {
            let parse = |value: &str| {
                u64::from_str_radix(value, 16)
                    .map_err(|_| format!("无效 diagnostic 十六进制字段: {value}"))
            };
            Ok(ChannelCounters {
                c1_errors: parse(fields[0])?,
                c1_uncorrectable: parse(fields[1])?,
                header_errors: parse(fields[2])?,
                write_pass_errors: parse(fields[3])?,
                ccps: parse(fields[4])?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChannelRate {
    pub channel: usize,
    pub log10_bit_error_rate: Option<f64>,
    pub ccp_advanced: bool,
}

pub fn rates(before: &[ChannelCounters], after: &[ChannelCounters]) -> Vec<ChannelRate> {
    before
        .iter()
        .zip(after)
        .enumerate()
        .map(|(channel, (before, after))| {
            let delta_ccps = after
                .ccps
                .checked_sub(before.ccps)
                .filter(|delta| *delta > 0);
            let delta_c1 = after.c1_errors.checked_sub(before.c1_errors);
            let ccp_advanced = delta_ccps.is_some();
            let rate = match delta_ccps.zip(delta_c1) {
                Some((ccps, c1)) => Some(((c1 as f64) / (ccps as f64) / 2.0 / 1920.0).log10()),
                // LTFSCopyGUI 对未前进的 channel 使用 -2.98 占位。
                None if after.ccps == before.ccps => Some(-2.98),
                None => None,
            };
            ChannelRate {
                channel,
                log10_bit_error_rate: rate,
                ccp_advanced,
            }
        })
        .collect()
}

pub fn worst_rate(rates: &[ChannelRate]) -> Option<f64> {
    if !rates.iter().any(|rate| rate.ccp_advanced) {
        return Some(0.0);
    }
    let mut result = rates
        .iter()
        .filter_map(|rate| rate.log10_bit_error_rate)
        .filter(|rate| *rate < 0.0)
        .max_by(f64::total_cmp)?;
    // LTFSCopyGUI 把极低值/负无穷归零，作为“没有可报告错误率”。
    if result < -10.0 {
        result = 0.0;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_calculates_gui_compatible_rate() {
        let body = b"7\r\n2B5\r\n0\r\n0\r\n0000000A\t0\t0\t0\t00000558\r\n";
        let mut page = vec![0x88, 0, 0, body.len() as u8];
        page.extend_from_slice(body);
        let before = parse_page(&page, PageKind::Write).unwrap();
        let mut after = before.clone();
        after[0].c1_errors += 10;
        after[0].ccps += 100;
        let result = rates(&before, &after);
        let expected = (10.0_f64 / 100.0 / 2.0 / 1920.0).log10();
        assert!((result[0].log10_bit_error_rate.unwrap() - expected).abs() < 1e-12);
        assert!(result[0].ccp_advanced);
        assert_eq!(worst_rate(&result), result[0].log10_bit_error_rate);
    }

    #[test]
    fn matches_gui_null_channel_and_zero_error_rules() {
        let before = vec![ChannelCounters::default(), ChannelCounters::default()];
        let mut after = before.clone();
        after[0].ccps = 10;
        let result = rates(&before, &after);
        assert_eq!(result[0].log10_bit_error_rate, Some(f64::NEG_INFINITY));
        assert_eq!(result[1].log10_bit_error_rate, Some(-2.98));
        assert_eq!(worst_rate(&result), Some(-2.98));

        let no_progress = rates(&before, &before);
        assert_eq!(worst_rate(&no_progress), Some(0.0));
    }
}
