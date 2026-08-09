//! LTFS label 解析。
//!
//! 每个分区 BOT 的磁带布局（LTFS Format Spec / OpenLTFS 实现）：
//!
//! ```text
//! [ANSI VOL1 label 记录(80B)][FileMark][LTFS XML label 记录][FileMark]
//! ```
//!
//! ANSI label 的 24-27 字节是签名 "LTFS"，4-9 字节是 6 位卷序列（barcode）。
//! XML label 顶层元素是 `<ltfslabel version="...">`，包含 creator、
//! formattime、volumeuuid、location/partition、partitions/index+data、
//! blocksize、compression 等必需字段。

use quick_xml::Reader;
use quick_xml::events::Event;

/// ANSI VOL1 label 固定长度。
pub const ANSI_LABEL_LEN: usize = 80;
/// ANSI label 中 LTFS 签名的位置（实现标识符前 4 字节）。
const ANSI_SIGNATURE_RANGE: std::ops::Range<usize> = 24..28;
/// ANSI label 中卷序列（barcode）的位置。
const ANSI_BARCODE_RANGE: std::ops::Range<usize> = 4..10;

/// 从 ANSI VOL1 label 记录中解析出的卷序列（barcode）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiLabel {
    pub barcode: String,
}

impl AnsiLabel {
    /// 解析 80 字节 VOL1 记录；记录不足 80 字节或签名不是 "LTFS" 时返回 None。
    pub fn parse(buf: &[u8]) -> Option<AnsiLabel> {
        if buf.len() < ANSI_LABEL_LEN {
            return None;
        }
        if &buf[ANSI_SIGNATURE_RANGE] != b"LTFS" {
            return None;
        }
        Some(AnsiLabel {
            barcode: ascii_field(&buf[ANSI_BARCODE_RANGE]),
        })
    }

    /// 生成 LTFS 使用的 80 字节 ANSI VOL1 label。
    pub fn to_bytes(&self) -> Result<[u8; ANSI_LABEL_LEN], LabelError> {
        if self.barcode.len() != 6
            || !self
                .barcode
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return Err(LabelError::InvalidBarcode);
        }
        let mut raw = [b' '; ANSI_LABEL_LEN];
        raw[..4].copy_from_slice(b"VOL1");
        raw[4..10].copy_from_slice(self.barcode.as_bytes());
        raw[10] = b'L';
        raw[24..28].copy_from_slice(b"LTFS");
        raw[79] = b'4';
        Ok(raw)
    }
}

/// LTFS XML label 解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// 顶层标签的 version 属性，如 "2.4.0"。
    pub version: String,
    pub creator: String,
    pub format_time: String,
    pub volume_uuid: String,
    pub blocksize: u64,
    pub compression: bool,
    /// 本 label 所在分区的逻辑编号。
    pub this_partition: u8,
    /// 逻辑 index 分区编号。
    pub index_partition: u8,
    /// 逻辑 data 分区编号。
    pub data_partition: u8,
}

/// label XML 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelError {
    Xml(String),
    NotLtfsLabel,
    MissingVersion,
    MissingField(&'static str),
    BadNumber(&'static str),
    InvalidBarcode,
}

impl std::fmt::Display for LabelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LabelError::Xml(e) => write!(f, "XML 解析失败: {e}"),
            LabelError::NotLtfsLabel => write!(f, "顶层元素不是 <ltfslabel>"),
            LabelError::MissingVersion => write!(f, "缺少 version 属性"),
            LabelError::MissingField(name) => write!(f, "缺少必需字段 <{name}>"),
            LabelError::BadNumber(name) => write!(f, "<{name}> 不是有效数字"),
            LabelError::InvalidBarcode => write!(f, "Barcode 必须是 6 位大写 ASCII 字母或数字"),
        }
    }
}

impl Label {
    /// 解析 LTFS label XML 文本。
    pub fn parse_xml(xml: &str) -> Result<Label, LabelError> {
        let mut reader = Reader::from_str(xml);
        // 实体引用会把一个字段拆成多个 Text/GeneralRef 事件；逐事件 trim
        // 会吞掉实体两侧的合法空格，因此在字段完整累积后再解释内容。
        reader.config_mut().trim_text(false);

        #[derive(Clone, Copy, PartialEq)]
        enum Section {
            Root,
            Location,
            Partitions,
        }

        let mut section = Section::Root;
        let mut elem = String::new();
        let mut field_text = String::new();
        let mut root_seen = false;

        let mut version = None;
        let mut creator = None;
        let mut format_time = None;
        let mut volume_uuid = None;
        let mut blocksize = None;
        let mut compression = None;
        let mut this_partition = None;
        let mut index_partition = None;
        let mut data_partition = None;

        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let name = element_name(e.name());
                    match name.as_str() {
                        "ltfslabel" => {
                            root_seen = true;
                            version = attr_value(&e, "version");
                        }
                        "location" => section = Section::Location,
                        "partitions" => section = Section::Partitions,
                        _ => {}
                    }
                    elem = name;
                    field_text.clear();
                }
                Ok(Event::Text(t)) => {
                    let text = unescape_text(&t)?;
                    field_text.push_str(&text);
                }
                Ok(Event::GeneralRef(r)) => {
                    field_text.push_str(&decode_general_ref(&r)?);
                }
                Ok(Event::End(e)) => {
                    if !field_text.is_empty() {
                        let text = field_text.clone();
                        match (section, elem.as_str()) {
                            (Section::Root, "creator") => creator = Some(text),
                            (Section::Root, "formattime") => format_time = Some(text),
                            (Section::Root, "volumeuuid") => volume_uuid = Some(text),
                            (Section::Root, "blocksize") => {
                                blocksize = Some(parse_u64(&text, "blocksize")?)
                            }
                            (Section::Root, "compression") => {
                                compression = Some(parse_bool(&text, "compression")?)
                            }
                            (Section::Location, "partition") => {
                                this_partition = Some(parse_partition(&text, "partition")?)
                            }
                            (Section::Partitions, "index") => {
                                index_partition = Some(parse_partition(&text, "index")?)
                            }
                            (Section::Partitions, "data") => {
                                data_partition = Some(parse_partition(&text, "data")?)
                            }
                            _ => {}
                        }
                    }
                    match element_name(e.name()).as_str() {
                        "location" | "partitions" => section = Section::Root,
                        _ => {}
                    }
                    elem.clear();
                    field_text.clear();
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(LabelError::Xml(e.to_string())),
                _ => {}
            }
        }

        if !root_seen {
            return Err(LabelError::NotLtfsLabel);
        }
        Ok(Label {
            version: version.ok_or(LabelError::MissingVersion)?,
            creator: creator.ok_or(LabelError::MissingField("creator"))?,
            format_time: format_time.ok_or(LabelError::MissingField("formattime"))?,
            volume_uuid: volume_uuid.ok_or(LabelError::MissingField("volumeuuid"))?,
            blocksize: blocksize.ok_or(LabelError::MissingField("blocksize"))?,
            compression: compression.ok_or(LabelError::MissingField("compression"))?,
            this_partition: this_partition.ok_or(LabelError::MissingField("location/partition"))?,
            index_partition: index_partition.ok_or(LabelError::MissingField("partitions/index"))?,
            data_partition: data_partition.ok_or(LabelError::MissingField("partitions/data"))?,
        })
    }

    /// 序列化为 LTFS 2.x XML label。
    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ltfslabel version=\"{}\">\n\
<creator>{}</creator>\n\
<formattime>{}</formattime>\n\
<volumeuuid>{}</volumeuuid>\n\
<location><partition>{}</partition></location>\n\
<partitions><index>{}</index><data>{}</data></partitions>\n\
<blocksize>{}</blocksize>\n\
<compression>{}</compression>\n\
</ltfslabel>\n",
            xml_escape(&self.version),
            xml_escape(&self.creator),
            xml_escape(&self.format_time),
            xml_escape(&self.volume_uuid),
            partition_name(self.this_partition),
            partition_name(self.index_partition),
            partition_name(self.data_partition),
            self.blocksize,
            self.compression,
        )
    }
}

fn partition_name(partition: u8) -> char {
    match partition {
        0 => 'a',
        1 => 'b',
        other => char::from(b'0'.saturating_add(other)),
    }
}

fn xml_escape(value: &str) -> String {
    quick_xml::escape::escape(value).into_owned()
}

fn element_name(name: quick_xml::name::QName<'_>) -> String {
    String::from_utf8_lossy(name.as_ref()).into_owned()
}

fn attr_value(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == name.as_bytes() {
            return Some(String::from_utf8_lossy(&attr.value).into_owned());
        }
    }
    None
}

fn unescape_text(t: &quick_xml::events::BytesText<'_>) -> Result<String, LabelError> {
    let raw = t.decode().map_err(|e| LabelError::Xml(e.to_string()))?;
    Ok(quick_xml::escape::unescape(&raw)
        .map_err(|e| LabelError::Xml(e.to_string()))?
        .to_string())
}

fn decode_general_ref(r: &quick_xml::events::BytesRef<'_>) -> Result<String, LabelError> {
    let name = r.decode().map_err(|e| LabelError::Xml(e.to_string()))?;
    quick_xml::escape::unescape(&format!("&{name};"))
        .map(|s| s.into_owned())
        .map_err(|e| LabelError::Xml(e.to_string()))
}

fn parse_u64(text: &str, field: &'static str) -> Result<u64, LabelError> {
    text.parse().map_err(|_| LabelError::BadNumber(field))
}

/// 解析 LTFS 逻辑分区标识：标准写法是 'a'/'b'，同时兼容数字 '0'/'1'。
fn parse_partition(text: &str, field: &'static str) -> Result<u8, LabelError> {
    match text {
        "a" | "0" => Ok(0),
        "b" | "1" => Ok(1),
        _ => Err(LabelError::BadNumber(field)),
    }
}

fn parse_bool(text: &str, field: &'static str) -> Result<bool, LabelError> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(LabelError::BadNumber(field)),
    }
}

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

    const LABEL_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ltfslabel version="2.4.0">
  <creator>IBM LTFS 2.4.8.4 (IBM LTFS)</creator>
  <formattime>2014-10-10T10:10:10.0000000+00:00</formattime>
  <volumeuuid>c72c9917-cf2e-4873-8b64-9ac05b2f324c</volumeuuid>
  <location>
    <partition>a</partition>
  </location>
  <partitions>
    <index>a</index>
    <data>b</data>
  </partitions>
  <blocksize>524288</blocksize>
  <compression>false</compression>
</ltfslabel>"#;

    /// 真实磁带上由 OpenLTFS mkltfs 2.4.8.4 写出的 label。
    const REAL_LABEL_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ltfslabel version="2.4.0">
    <creator>IBM LTFS 2.4.8.4 (Prelim) - Linux - mkltfs</creator>
    <formattime>2026-08-09T09:29:05.552272521Z</formattime>
    <volumeuuid>0cc710d4-bc2f-4c54-823e-a44413987f5d</volumeuuid>
    <location>
        <partition>a</partition>
    </location>
    <partitions>
        <index>a</index>
        <data>b</data>
    </partitions>
    <blocksize>524288</blocksize>
    <compression>true</compression>
</ltfslabel>"#;

    #[test]
    fn parse_ansi_label() {
        let mut buf = [b' '; 80];
        buf[..4].copy_from_slice(b"VOL1");
        buf[4..10].copy_from_slice(b"E6008A");
        buf[24..28].copy_from_slice(b"LTFS");
        let label = AnsiLabel::parse(&buf).unwrap();
        assert_eq!(label.barcode, "E6008A");
    }

    #[test]
    fn ansi_label_rejects_non_ltfs_signature() {
        let mut buf = [b' '; 80];
        buf[..4].copy_from_slice(b"VOL1");
        buf[24..28].copy_from_slice(b"XXXX");
        assert_eq!(AnsiLabel::parse(&buf), None);
        assert_eq!(AnsiLabel::parse(&buf[..40]), None);
    }

    #[test]
    fn ansi_label_generation_round_trips() {
        let raw = AnsiLabel {
            barcode: "E6008A".into(),
        }
        .to_bytes()
        .unwrap();
        assert_eq!(raw.len(), 80);
        assert_eq!(&raw[..4], b"VOL1");
        assert_eq!(&raw[24..28], b"LTFS");
        assert_eq!(raw[79], b'4');
        assert_eq!(AnsiLabel::parse(&raw).unwrap().barcode, "E6008A");
        assert!(
            AnsiLabel {
                barcode: "bad".into()
            }
            .to_bytes()
            .is_err()
        );
    }

    #[test]
    fn parse_label_xml() {
        let label = Label::parse_xml(LABEL_XML).unwrap();
        assert_eq!(label.version, "2.4.0");
        assert_eq!(label.creator, "IBM LTFS 2.4.8.4 (IBM LTFS)");
        assert_eq!(label.volume_uuid, "c72c9917-cf2e-4873-8b64-9ac05b2f324c");
        assert_eq!(label.format_time, "2014-10-10T10:10:10.0000000+00:00");
        assert_eq!(label.blocksize, 524288);
        assert!(!label.compression);
        assert_eq!(label.this_partition, 0);
        assert_eq!(label.index_partition, 0);
        assert_eq!(label.data_partition, 1);
    }

    #[test]
    fn label_xml_rejects_wrong_top_tag() {
        let xml = LABEL_XML.replace("<ltfslabel", "<notlabel");
        assert!(Label::parse_xml(&xml).is_err());
    }

    #[test]
    fn label_xml_missing_required_field() {
        let xml = LABEL_XML.replace("<creator>IBM LTFS 2.4.8.4 (IBM LTFS)</creator>", "");
        assert_eq!(
            Label::parse_xml(&xml),
            Err(LabelError::MissingField("creator"))
        );
    }

    #[test]
    fn label_xml_handles_compression_true() {
        let xml = LABEL_XML.replace(
            "<compression>false</compression>",
            "<compression>true</compression>",
        );
        assert!(Label::parse_xml(&xml).unwrap().compression);
    }

    #[test]
    fn parse_real_mkltfs_label() {
        let label = Label::parse_xml(REAL_LABEL_XML).unwrap();
        assert_eq!(label.version, "2.4.0");
        assert_eq!(label.creator, "IBM LTFS 2.4.8.4 (Prelim) - Linux - mkltfs");
        assert_eq!(label.volume_uuid, "0cc710d4-bc2f-4c54-823e-a44413987f5d");
        assert_eq!(label.this_partition, 0);
        assert_eq!(label.index_partition, 0);
        assert_eq!(label.data_partition, 1);
        assert_eq!(label.blocksize, 524288);
        assert!(label.compression);
    }

    #[test]
    fn generated_label_round_trips_and_escapes_text() {
        let mut label = Label::parse_xml(REAL_LABEL_XML).unwrap();
        label.creator = "tapecpy & test".into();
        let xml = label.to_xml();
        assert!(xml.contains("tapecpy &amp; test"));
        assert_eq!(Label::parse_xml(&xml).unwrap(), label);
    }

    #[test]
    fn partition_accepts_digit_form_too() {
        let xml = REAL_LABEL_XML
            .replace("<partition>a</partition>", "<partition>0</partition>")
            .replace("<index>a</index>", "<index>0</index>")
            .replace("<data>b</data>", "<data>1</data>");
        let label = Label::parse_xml(&xml).unwrap();
        assert_eq!(label.this_partition, 0);
        assert_eq!(label.index_partition, 0);
        assert_eq!(label.data_partition, 1);
    }
}
