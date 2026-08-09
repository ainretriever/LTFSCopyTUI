//! LTFS index 解析。
//!
//! 顶层元素是 `<ltfsindex version="...">`，必需字段包括 creator、
//! volumeuuid、generationnumber、updatetime、location、allowpolicyupdate、
//! directory；可选字段包括 previousgenerationlocation、dataplacementpolicy、
//! comment、volumelockstate、highestfileuid 等（参考 LTFS Format Spec 与
//! OpenLTFS `xml_reader_libltfs.c`）。

use quick_xml::Reader;
use quick_xml::events::Event;

/// index 中的磁带位置（partition + startblock）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapePos {
    pub partition: u8,
    pub startblock: u64,
}

/// LTFS index 摘要（Milestone 2 只关心 volume 基本信息，不展开目录树）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub version: String,
    pub creator: String,
    pub volume_uuid: String,
    /// 根目录 name 属性（LTFS 约定为 volume name）。
    pub volume_name: Option<String>,
    pub generation: u64,
    pub update_time: String,
    pub self_location: TapePos,
    pub previous_location: Option<TapePos>,
    pub allow_policy_update: bool,
    /// unlocked / locked / permlocked（absent 时为 None）。
    pub volume_lock_state: Option<String>,
    pub highest_file_uid: Option<u64>,
    /// `<file>` 与 `<symlink>` 元素数量（不含目录）。
    pub file_count: u64,
    /// `<directory>` 元素数量（不含根目录）。
    pub directory_count: u64,
}

/// index XML 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    Xml(String),
    NotLtfsIndex,
    MissingVersion,
    MissingField(&'static str),
    BadNumber(&'static str),
    BadBool(&'static str),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Xml(e) => write!(f, "XML 解析失败: {e}"),
            IndexError::NotLtfsIndex => write!(f, "顶层元素不是 <ltfsindex>"),
            IndexError::MissingVersion => write!(f, "缺少 version 属性"),
            IndexError::MissingField(name) => write!(f, "缺少必需字段 <{name}>"),
            IndexError::BadNumber(name) => write!(f, "<{name}> 不是有效数字"),
            IndexError::BadBool(name) => write!(f, "<{name}> 不是有效布尔值"),
        }
    }
}

impl Index {
    /// 解析 LTFS index XML 文本。
    pub fn parse_xml(xml: &str) -> Result<Index, IndexError> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);

        #[derive(Clone, Copy, PartialEq)]
        enum Section {
            Root,
            Location,
            PrevLocation,
            Directory,
        }

        let mut section = Section::Root;
        let mut elem = String::new();
        let mut root_seen = false;
        let mut in_directory = false;
        let mut dir_depth = 0u64;
        let mut directory_count = 0u64;
        let mut file_count = 0u64;

        let mut version = None;
        let mut creator = None;
        let mut volume_uuid = None;
        let mut volume_name = None;
        let mut generation = None;
        let mut update_time = None;
        let mut allow_policy_update = None;
        let mut volume_lock_state = None;
        let mut highest_file_uid = None;
        let mut self_partition = None;
        let mut self_block = None;
        let mut prev_partition = None;
        let mut prev_block = None;

        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let name = element_name(e.name());
                    match name.as_str() {
                        "ltfsindex" => {
                            root_seen = true;
                            version = attr_value(&e, "version");
                        }
                        "location" if section == Section::Root => {
                            section = Section::Location
                        }
                        "previousgenerationlocation" if section == Section::Root => {
                            section = Section::PrevLocation
                        }
                        "directory" if section == Section::Root => {
                            section = Section::Directory;
                            in_directory = true;
                            dir_depth = 1;
                            volume_name = attr_value(&e, "name");
                        }
                        _ => {
                            if in_directory {
                                match name.as_str() {
                                    "directory" => {
                                        directory_count += 1;
                                        dir_depth += 1;
                                    }
                                    "file" | "symlink" => file_count += 1,
                                    _ => {}
                                }
                            }
                        }
                    }
                    elem = name;
                }
                Ok(Event::Text(t)) => {
                    let text = unescape_text(&t)?;
                    if text.is_empty() {
                        continue;
                    }
                    if in_directory {
                        // 根目录的 <name> 子元素是 LTFS volume name；
                        // 根目录 name 在 <contents> 之前，取第一个即可。
                        if volume_name.is_none() && dir_depth == 1 && elem == "name" {
                            volume_name = Some(text);
                        }
                        continue;
                    }
                    match (section, elem.as_str()) {
                        (Section::Root, "creator") => creator = Some(text),
                        (Section::Root, "volumeuuid") => volume_uuid = Some(text),
                        (Section::Root, "generationnumber") => {
                            generation = Some(parse_u64(&text, "generationnumber")?)
                        }
                        (Section::Root, "updatetime") => update_time = Some(text),
                        (Section::Root, "allowpolicyupdate") => {
                            allow_policy_update = Some(parse_bool(&text, "allowpolicyupdate")?)
                        }
                        (Section::Root, "volumelockstate") => volume_lock_state = Some(text),
                        (Section::Root, "highestfileuid") => {
                            highest_file_uid = Some(parse_u64(&text, "highestfileuid")?)
                        }
                        (Section::Location, "partition") => {
                            self_partition = Some(parse_partition(&text, "partition")?)
                        }
                        (Section::Location, "startblock") => {
                            self_block = Some(parse_u64(&text, "startblock")?)
                        }
                        (Section::PrevLocation, "partition") => {
                            prev_partition = Some(parse_partition(&text, "partition")?)
                        }
                        (Section::PrevLocation, "startblock") => {
                            prev_block = Some(parse_u64(&text, "startblock")?)
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(e)) => {
                    match element_name(e.name()).as_str() {
                        "location" if section == Section::Location => section = Section::Root,
                        "previousgenerationlocation" if section == Section::PrevLocation => {
                            section = Section::Root
                        }
                        "directory" if in_directory => {
                            dir_depth -= 1;
                            if dir_depth == 0 {
                                in_directory = false;
                                section = Section::Root;
                            }
                        }
                        _ => {}
                    }
                    elem.clear();
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(IndexError::Xml(e.to_string())),
                _ => {}
            }
        }

        if !root_seen {
            return Err(IndexError::NotLtfsIndex);
        }
        let self_location = TapePos {
            partition: self_partition.ok_or(IndexError::MissingField("location/partition"))?,
            startblock: self_block.ok_or(IndexError::MissingField("location/startblock"))?,
        };
        let previous_location = match (prev_partition, prev_block) {
            (Some(partition), Some(startblock)) => Some(TapePos { partition, startblock }),
            _ => None,
        };
        Ok(Index {
            version: version.ok_or(IndexError::MissingVersion)?,
            creator: creator.ok_or(IndexError::MissingField("creator"))?,
            volume_uuid: volume_uuid.ok_or(IndexError::MissingField("volumeuuid"))?,
            volume_name,
            generation: generation.ok_or(IndexError::MissingField("generationnumber"))?,
            update_time: update_time.ok_or(IndexError::MissingField("updatetime"))?,
            self_location,
            previous_location,
            allow_policy_update: allow_policy_update
                .ok_or(IndexError::MissingField("allowpolicyupdate"))?,
            volume_lock_state,
            highest_file_uid,
            file_count,
            directory_count,
        })
    }
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

fn unescape_text(t: &quick_xml::events::BytesText<'_>) -> Result<String, IndexError> {
    let raw = t.decode().map_err(|e| IndexError::Xml(e.to_string()))?;
    Ok(quick_xml::escape::unescape(&raw)
        .map_err(|e| IndexError::Xml(e.to_string()))?
        .trim()
        .to_string())
}

fn parse_u64(text: &str, field: &'static str) -> Result<u64, IndexError> {
    text.parse().map_err(|_| IndexError::BadNumber(field))
}

/// 解析 LTFS 逻辑分区标识：标准写法是 'a'/'b'，同时兼容数字 '0'/'1'。
fn parse_partition(text: &str, field: &'static str) -> Result<u8, IndexError> {
    match text {
        "a" | "0" => Ok(0),
        "b" | "1" => Ok(1),
        _ => Err(IndexError::BadNumber(field)),
    }
}

fn parse_bool(text: &str, field: &'static str) -> Result<bool, IndexError> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(IndexError::BadBool(field)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ltfsindex version="2.4.0">
  <creator>IBM LTFS 2.4.8.4 (IBM LTFS)</creator>
  <volumeuuid>c72c9917-cf2e-4873-8b64-9ac05b2f324c</volumeuuid>
  <generationnumber>2</generationnumber>
  <updatetime>2026-06-18T11:28:00.0000000+00:00</updatetime>
  <location>
    <partition>a</partition>
    <startblock>5</startblock>
  </location>
  <previousgenerationlocation>
    <partition>a</partition>
    <startblock>4</startblock>
  </previousgenerationlocation>
  <allowpolicyupdate>false</allowpolicyupdate>
  <highestfileuid>4</highestfileuid>
  <volumelockstate>unlocked</volumelockstate>
  <directory>
    <name>/</name>
    <fileuid>1</fileuid>
    <contents>
      <file>
        <name>notes.txt</name>
        <fileuid>2</fileuid>
        <length>1024</length>
        <creationtime>2026-06-18T11:28:00.0000000+00:00</creationtime>
        <changetime>2026-06-18T11:28:00.0000000+00:00</changetime>
        <extentinfo>
          <extent>
            <fileoffset>0</fileoffset>
            <byteoffset>0</byteoffset>
            <bytecount>1024</bytecount>
            <partition>b</partition>
          </extent>
        </extentinfo>
      </file>
      <directory>
        <name>sub</name>
        <fileuid>3</fileuid>
        <contents>
          <file>
            <name>inner.bin</name>
            <fileuid>4</fileuid>
            <length>2048</length>
          </file>
        </contents>
      </directory>
      <symlink>
        <name>link</name>
        <target>/notes.txt</target>
      </symlink>
    </contents>
  </directory>
</ltfsindex>"#;

    /// 真实磁带上由 OpenLTFS mkltfs 2.4.8.4 写出的 index。
    const REAL_INDEX_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ltfsindex version="2.4.0">
<creator>IBM LTFS 2.4.8.4 (Prelim) - Linux - mkltfs - Format</creator>
<volumeuuid>0cc710d4-bc2f-4c54-823e-a44413987f5d</volumeuuid>
<generationnumber>1</generationnumber>
<updatetime>2026-08-09T09:29:32.530506355Z</updatetime>
<location>
<partition>a</partition>
<startblock>5</startblock>
</location>
<previousgenerationlocation>
<partition>b</partition>
<startblock>5</startblock>
</previousgenerationlocation>
<allowpolicyupdate>true</allowpolicyupdate>
<highestfileuid>1</highestfileuid>
<volumelockstate>unlocked</volumelockstate>
<directory>
<name>tapecpy M2 test</name>
<readonly>false</readonly>
<creationtime>2026-08-09T09:29:05.552272521Z</creationtime>
<changetime>2026-08-09T09:29:05.552272521Z</changetime>
<modifytime>2026-08-09T09:29:05.552272521Z</modifytime>
<accesstime>2026-08-09T09:29:05.552272521Z</accesstime>
<backuptime>2026-08-09T09:29:05.552272521Z</backuptime>
<fileuid>1</fileuid>
<contents/>
</directory>
</ltfsindex>"#;

    #[test]
    fn parse_index_xml() {
        let idx = Index::parse_xml(INDEX_XML).unwrap();
        assert_eq!(idx.version, "2.4.0");
        assert_eq!(idx.volume_name.as_deref(), Some("/"));
        assert_eq!(idx.generation, 2);
        assert_eq!(idx.self_location, TapePos { partition: 0, startblock: 5 });
        assert_eq!(
            idx.previous_location,
            Some(TapePos { partition: 0, startblock: 4 })
        );
        assert!(!idx.allow_policy_update);
        assert_eq!(idx.volume_lock_state.as_deref(), Some("unlocked"));
        assert_eq!(idx.highest_file_uid, Some(4));
        assert_eq!(idx.file_count, 3); // notes.txt + inner.bin + symlink
        assert_eq!(idx.directory_count, 1); // 子目录 sub，不含根
    }

    #[test]
    fn index_xml_rejects_wrong_top_tag() {
        let xml = INDEX_XML.replace("<ltfsindex", "<notindex");
        assert!(Index::parse_xml(&xml).is_err());
    }

    #[test]
    fn index_xml_missing_generation() {
        let xml = INDEX_XML.replace("<generationnumber>2</generationnumber>", "");
        assert_eq!(
            Index::parse_xml(&xml),
            Err(IndexError::MissingField("generationnumber"))
        );
    }

    #[test]
    fn index_xml_without_previous_location() {
        let xml = INDEX_XML.replace(
            "<previousgenerationlocation>\n    <partition>a</partition>\n    <startblock>4</startblock>\n  </previousgenerationlocation>\n",
            "",
        );
        let idx = Index::parse_xml(&xml).unwrap();
        assert_eq!(idx.previous_location, None);
    }

    #[test]
    fn index_xml_empty_directory() {
        let xml = INDEX_XML.replace(
            "<file>\n        <name>notes.txt</name>\n        <fileuid>2</fileuid>\n        <length>1024</length>\n        <creationtime>2026-06-18T11:28:00.0000000+00:00</creationtime>\n        <changetime>2026-06-18T11:28:00.0000000+00:00</changetime>\n        <extentinfo>\n          <extent>\n            <fileoffset>0</fileoffset>\n            <byteoffset>0</byteoffset>\n            <bytecount>1024</bytecount>\n            <partition>b</partition>\n          </extent>\n        </extentinfo>\n      </file>\n",
            "",
        );
        let idx = Index::parse_xml(&xml).unwrap();
        assert_eq!(idx.file_count, 2);
    }

    #[test]
    fn parse_real_mkltfs_index() {
        let idx = Index::parse_xml(REAL_INDEX_XML).unwrap();
        assert_eq!(idx.volume_name.as_deref(), Some("tapecpy M2 test"));
        assert_eq!(idx.creator, "IBM LTFS 2.4.8.4 (Prelim) - Linux - mkltfs - Format");
        assert_eq!(idx.volume_uuid, "0cc710d4-bc2f-4c54-823e-a44413987f5d");
        assert_eq!(idx.generation, 1);
        assert_eq!(idx.self_location, TapePos { partition: 0, startblock: 5 });
        assert_eq!(
            idx.previous_location,
            Some(TapePos { partition: 1, startblock: 5 })
        );
        assert!(idx.allow_policy_update);
        assert_eq!(idx.highest_file_uid, Some(1));
        assert_eq!(idx.volume_lock_state.as_deref(), Some("unlocked"));
        assert_eq!(idx.file_count, 0);
        assert_eq!(idx.directory_count, 0);
    }
}
