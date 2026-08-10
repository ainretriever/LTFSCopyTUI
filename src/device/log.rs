//! SSC LOG SENSE page 的纯数据解析。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogParameter {
    pub code: u16,
    pub control: u8,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TruncatedHeader,
    WrongPage { expected: u8, actual: u8 },
    TruncatedPage { declared: usize, actual: usize },
    TruncatedParameter { code: u16 },
}

pub fn parse_page(data: &[u8], expected_page: u8) -> Result<Vec<LogParameter>, ParseError> {
    if data.len() < 4 {
        return Err(ParseError::TruncatedHeader);
    }
    let actual_page = data[0] & 0x3f;
    if actual_page != expected_page {
        return Err(ParseError::WrongPage {
            expected: expected_page,
            actual: actual_page,
        });
    }
    let declared = u16::from_be_bytes([data[2], data[3]]) as usize;
    let end = 4usize.saturating_add(declared);
    if data.len() < end {
        return Err(ParseError::TruncatedPage {
            declared: end,
            actual: data.len(),
        });
    }
    let mut parameters = Vec::new();
    let mut offset = 4;
    while offset < end {
        if end - offset < 4 {
            return Err(ParseError::TruncatedParameter { code: 0 });
        }
        let code = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let len = data[offset + 3] as usize;
        if end - offset - 4 < len {
            return Err(ParseError::TruncatedParameter { code });
        }
        parameters.push(LogParameter {
            code,
            control: data[offset + 2],
            value: data[offset + 4..offset + 4 + len].to_vec(),
        });
        offset += 4 + len;
    }
    Ok(parameters)
}

pub fn unsigned_value(value: &[u8]) -> Option<u64> {
    if value.is_empty() || value.len() > 8 {
        return None;
    }
    Some(
        value
            .iter()
            .fold(0u64, |n, byte| (n << 8) | u64::from(*byte)),
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorCounters {
    pub corrected_without_delay: Option<u64>,
    pub corrected_with_delay: Option<u64>,
    pub total_corrected: Option<u64>,
    pub correction_processed: Option<u64>,
    pub data_processed: Option<u64>,
    pub uncorrected: Option<u64>,
}

pub fn error_counters(parameters: &[LogParameter]) -> ErrorCounters {
    let mut counters = ErrorCounters::default();
    for parameter in parameters {
        let value = unsigned_value(&parameter.value);
        match parameter.code {
            0 => counters.corrected_without_delay = value,
            1 => counters.corrected_with_delay = value,
            3 => counters.total_corrected = value,
            4 => counters.correction_processed = value,
            5 => counters.data_processed = value,
            6 => counters.uncorrected = value,
            _ => {}
        }
    }
    counters
}

/// TapeAlert page 2Eh 中值非零的 parameter code（标准 flag 编号 1..64）。
pub fn active_tape_alerts(parameters: &[LogParameter]) -> Vec<u16> {
    parameters
        .iter()
        .filter(|parameter| unsigned_value(&parameter.value).is_some_and(|value| value != 0))
        .map(|parameter| parameter.code)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_variable_length_parameters() {
        let page = [0x02, 0, 0, 13, 0, 3, 0, 4, 0, 0, 0, 7, 0, 6, 0, 1, 2];
        let parameters = parse_page(&page, 0x02).unwrap();
        assert_eq!(parameters.len(), 2);
        let counters = error_counters(&parameters);
        assert_eq!(counters.total_corrected, Some(7));
        assert_eq!(counters.uncorrected, Some(2));
    }

    #[test]
    fn rejects_truncated_parameter() {
        let page = [0x2e, 0, 0, 5, 0, 1, 0, 2, 1];
        assert_eq!(
            parse_page(&page, 0x2e),
            Err(ParseError::TruncatedParameter { code: 1 })
        );
    }
}
