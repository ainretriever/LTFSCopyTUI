//! LTFS 使用的 MAM attribute 值编码。
//!
//! 本模块只处理纯数据，不发送 SCSI 命令。READ/WRITE ATTRIBUTE 由设备层负责，
//! flush、VCR 读取和提交顺序由 Application 层负责。

pub const VOLUME_CHANGE_REFERENCE: u16 = 0x0009;
pub const APPLICATION_VENDOR: u16 = 0x0800;
pub const APPLICATION_NAME: u16 = 0x0801;
pub const APPLICATION_VERSION: u16 = 0x0802;
pub const USER_MEDIUM_TEXT_LABEL: u16 = 0x0803;
pub const DATE_TIME_LAST_WRITTEN: u16 = 0x0804;
pub const TEXT_LOCALIZATION_IDENTIFIER: u16 = 0x0805;
pub const BARCODE: u16 = 0x0806;
pub const APPLICATION_FORMAT_VERSION: u16 = 0x080b;
pub const VOLUME_COHERENCY_INFORMATION: u16 = 0x080c;
pub const MEDIUM_GLOBALLY_UNIQUE_IDENTIFIER: u16 = 0x0820;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueFormat {
    Binary,
    Ascii,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeValue {
    pub id: u16,
    pub format: ValueFormat,
    pub value: Vec<u8>,
    pub required: bool,
}

/// 生成 LTFS 2.4 第 10.4 节定义的格式化时 Host-type attributes。
pub fn format_host_attributes(
    volume_name: &str,
    barcode: &str,
    volume_uuid: &str,
    app_version: &str,
    written_time: &str,
) -> Vec<AttributeValue> {
    vec![
        ascii(APPLICATION_VENDOR, "tapecpy", 8, true),
        // LTFS 2.4 §10.4.2 要求 Application Name 以 "LTFS " 开头。
        ascii(APPLICATION_NAME, "LTFS tapecpy", 32, true),
        ascii(APPLICATION_VERSION, app_version, 8, true),
        AttributeValue {
            id: USER_MEDIUM_TEXT_LABEL,
            format: ValueFormat::Text,
            value: null_terminated_utf8(volume_name, 160),
            required: false,
        },
        AttributeValue {
            id: TEXT_LOCALIZATION_IDENTIFIER,
            format: ValueFormat::Binary,
            value: vec![0x81], // SPC UTF-8
            required: false,
        },
        ascii(BARCODE, barcode, 32, false),
        ascii(APPLICATION_FORMAT_VERSION, "2.4.0", 16, true),
        ascii(DATE_TIME_LAST_WRITTEN, written_time, 12, false),
        AttributeValue {
            id: MEDIUM_GLOBALLY_UNIQUE_IDENTIFIER,
            format: ValueFormat::Binary,
            value: volume_uuid.as_bytes().to_vec(),
            required: false,
        },
    ]
}

fn ascii(id: u16, value: &str, len: usize, required: bool) -> AttributeValue {
    let mut out = vec![b' '; len];
    let bytes = value.as_bytes();
    let n = bytes.len().min(len);
    out[..n].copy_from_slice(&bytes[..n]);
    AttributeValue {
        id,
        format: ValueFormat::Ascii,
        value: out,
        required,
    }
}

fn null_terminated_utf8(value: &str, len: usize) -> Vec<u8> {
    let mut out = vec![0; len];
    if len == 0 {
        return out;
    }
    let mut end = value.len().min(len - 1);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    out[..end].copy_from_slice(&value.as_bytes()[..end]);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeCoherencyInformation {
    pub vcr: Vec<u8>,
    pub generation: u64,
    pub block: u64,
    pub volume_uuid: String,
    pub acsi_version: u8,
}

impl VolumeCoherencyInformation {
    /// 构造 OpenLTFS/LTFSCopyGUI 兼容的 8-byte VCR VCI。
    ///
    /// 部分 LTO 驱动器用 4 bytes 返回 VCR；SSC VCI 实现通常使用 8-byte
    /// coherency reference，因此将短值按大端整数左侧补零。
    pub fn new(vcr: &[u8], generation: u64, block: u64, volume_uuid: &str) -> Result<Self, String> {
        validate_uuid(volume_uuid)?;
        if block == 0 {
            return Err("VCI index block 不能为 0".into());
        }
        if vcr.is_empty() || vcr.iter().all(|b| *b == 0) || vcr.iter().all(|b| *b == 0xff) {
            return Err("VCR 无效（为空、全零或全 FF）".into());
        }
        let mut normalized = vec![0u8; 8];
        let copy = vcr.len().min(8);
        normalized[8 - copy..].copy_from_slice(&vcr[vcr.len() - copy..]);
        Ok(Self {
            vcr: normalized,
            generation,
            block,
            volume_uuid: volume_uuid.into(),
            acsi_version: 1,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        validate_uuid(&self.volume_uuid)?;
        if self.vcr.is_empty() || self.vcr.len() > u8::MAX as usize {
            return Err("VCI VCR 长度无效".into());
        }
        let mut out = Vec::with_capacity(1 + self.vcr.len() + 8 + 8 + 2 + 43);
        out.push(self.vcr.len() as u8);
        out.extend_from_slice(&self.vcr);
        out.extend_from_slice(&self.generation.to_be_bytes());
        out.extend_from_slice(&self.block.to_be_bytes());
        out.extend_from_slice(&43u16.to_be_bytes());
        out.extend_from_slice(b"LTFS\0");
        out.extend_from_slice(self.volume_uuid.as_bytes());
        out.push(0);
        out.push(self.acsi_version);
        Ok(out)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let vcr_len = *bytes.first().ok_or("VCI 为空")? as usize;
        let fixed = 1 + vcr_len + 8 + 8 + 2;
        if bytes.len() < fixed {
            return Err("VCI 在固定字段中截断".into());
        }
        let generation = u64::from_be_bytes(bytes[1 + vcr_len..9 + vcr_len].try_into().unwrap());
        let block = u64::from_be_bytes(bytes[9 + vcr_len..17 + vcr_len].try_into().unwrap());
        let acsi_len =
            u16::from_be_bytes(bytes[17 + vcr_len..19 + vcr_len].try_into().unwrap()) as usize;
        if bytes.len() < fixed + acsi_len || acsi_len < 43 {
            return Err("VCI ACSI 截断或长度不足".into());
        }
        let acsi = &bytes[fixed..fixed + acsi_len];
        if &acsi[..5] != b"LTFS\0" || acsi[41] != 0 {
            return Err("VCI ACSI 不是 LTFS 格式".into());
        }
        let volume_uuid = std::str::from_utf8(&acsi[5..41])
            .map_err(|_| "VCI UUID 不是 ASCII/UTF-8")?
            .to_string();
        validate_uuid(&volume_uuid)?;
        Ok(Self {
            vcr: bytes[1..1 + vcr_len].to_vec(),
            generation,
            block,
            volume_uuid,
            acsi_version: acsi[42],
        })
    }
}

fn validate_uuid(value: &str) -> Result<(), String> {
    if value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        Ok(())
    } else {
        Err("volume UUID 格式无效".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "90492284-35d4-436a-bbee-12121ae0fbac";

    #[test]
    fn vci_round_trip_and_layout() {
        let vci = VolumeCoherencyInformation::new(&[1, 2, 3, 4], 7, 42, UUID).unwrap();
        let bytes = vci.to_bytes().unwrap();
        assert_eq!(bytes.len(), 70);
        assert_eq!(bytes[0], 8);
        assert_eq!(&bytes[1..9], &[0, 0, 0, 0, 1, 2, 3, 4]);
        assert_eq!(&bytes[27..32], b"LTFS\0");
        assert_eq!(VolumeCoherencyInformation::parse(&bytes).unwrap(), vci);
    }

    #[test]
    fn rejects_invalid_vcr_and_block() {
        assert!(VolumeCoherencyInformation::new(&[0; 4], 1, 5, UUID).is_err());
        assert!(VolumeCoherencyInformation::new(&[0xff; 4], 1, 5, UUID).is_err());
        assert!(VolumeCoherencyInformation::new(&[1], 1, 0, UUID).is_err());
    }

    #[test]
    fn host_attributes_follow_ltfs_24_rules() {
        let attrs = format_host_attributes("卷名", "E6008A", UUID, "0.1.0", "202608101234");
        let app = attrs.iter().find(|a| a.id == APPLICATION_NAME).unwrap();
        assert!(app.value.starts_with(b"LTFS tapecpy"));
        let name = attrs
            .iter()
            .find(|a| a.id == USER_MEDIUM_TEXT_LABEL)
            .unwrap();
        let encoded_name = "卷名".as_bytes();
        assert_eq!(&name.value[..encoded_name.len()], encoded_name);
        assert_eq!(name.value[encoded_name.len()], 0);
        assert!(
            attrs
                .iter()
                .any(|a| a.id == MEDIUM_GLOBALLY_UNIQUE_IDENTIFIER)
        );
    }
}
