//! LTFS index 解析：摘要字段 + 完整目录树。
//!
//! 顶层元素是 `<ltfsindex version="...">`。目录树中每个条目（文件/目录/
//! 符号链接）的 `<name>` 都是子元素而非属性；根目录的 `<name>` 即
//! LTFS volume name（参考真实 OpenLTFS mkltfs 输出与 LTFS Format Spec）。

use quick_xml::Reader;
use quick_xml::events::Event;

/// index 中的磁带位置（partition + startblock）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapePos {
    pub partition: u8,
    pub startblock: u64,
}

/// 文件/目录的时间戳（LTFS XML 原样保留字符串）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTimes {
    pub creation_time: Option<String>,
    pub change_time: Option<String>,
    pub modify_time: Option<String>,
    pub access_time: Option<String>,
    pub backup_time: Option<String>,
}

/// 文件在数据分区上的 extent。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extent {
    /// 文件内逻辑偏移（块）。
    pub file_offset: u64,
    /// 所在分区（逻辑）。
    pub partition: u8,
    /// 数据起始磁带块号（记录索引）。
    pub start_block: u64,
    /// extent 字节长度（通常等于文件长度）。
    pub byte_count: u64,
}

/// LTFS extended attribute。`value_type` 缺省时按规范视为 `text`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtendedAttribute {
    pub key: String,
    pub value: String,
    pub value_type: Option<String>,
}

/// LTFS 文件条目。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub fileuid: u64,
    pub length: u64,
    pub readonly: bool,
    pub times: FileTimes,
    pub extents: Vec<Extent>,
    pub extended_attributes: Vec<ExtendedAttribute>,
    /// 符号链接目标；为 Some 时该条目是符号链接（LTFS 格式中 symlink
    /// 是 `<file>` 内的 `<symlink>` 子元素）。
    pub symlink_target: Option<String>,
}

/// LTFS 目录条目。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Directory {
    pub name: String,
    pub fileuid: u64,
    pub readonly: bool,
    pub times: FileTimes,
    pub extended_attributes: Vec<ExtendedAttribute>,
    pub entries: Vec<DirectoryEntry>,
}

/// 目录内的条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryEntry {
    File(FileEntry),
    Directory(Directory),
}

impl DirectoryEntry {
    pub fn name(&self) -> &str {
        match self {
            DirectoryEntry::File(f) => &f.name,
            DirectoryEntry::Directory(d) => &d.name,
        }
    }
}

/// LTFS index（Milestone 2 摘要 + Milestone 3 目录树）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub version: String,
    pub creator: String,
    pub volume_uuid: String,
    pub generation: u64,
    pub update_time: String,
    pub self_location: TapePos,
    pub previous_location: Option<TapePos>,
    pub allow_policy_update: bool,
    /// unlocked / locked / permlocked（absent 时为 None）。
    pub volume_lock_state: Option<String>,
    pub highest_file_uid: Option<u64>,
    /// 根目录（其 name 即 LTFS volume name）。
    pub root: Directory,
}

impl Index {
    /// 卷名 = 根目录 name。
    pub fn volume_name(&self) -> Option<&str> {
        if self.root.name.is_empty() {
            None
        } else {
            Some(&self.root.name)
        }
    }

    /// 树中的文件数（含符号链接）。
    pub fn file_count(&self) -> u64 {
        let mut n = 0;
        count_entries(&self.root, &mut n, &mut 0);
        n
    }

    /// 树中的目录数（不含根目录）。
    pub fn directory_count(&self) -> u64 {
        let mut n = 0;
        count_entries(&self.root, &mut 0, &mut n);
        n
    }

    /// 按路径查找目录（"/" 或空串为根目录）。
    pub fn find_directory(&self, path: &str) -> Option<&Directory> {
        let mut current = &self.root;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let next = current.entries.iter().find_map(|e| match e {
                DirectoryEntry::Directory(d) if d.name == part => Some(d),
                _ => None,
            })?;
            current = next;
        }
        Some(current)
    }

    /// 可变版本的 `find_directory`。
    pub fn find_directory_mut(&mut self, path: &str) -> Option<&mut Directory> {
        let mut current = &mut self.root;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let next = current.entries.iter_mut().find_map(|e| match e {
                DirectoryEntry::Directory(d) if d.name == part => Some(d),
                _ => None,
            })?;
            current = next;
        }
        Some(current)
    }

    /// 按路径查找普通文件（最后一个路径分量是文件名）。
    pub fn find_file(&self, path: &str) -> Option<&FileEntry> {
        let trimmed = path.trim_matches('/');
        let (dir_path, name) = match trimmed.rfind('/') {
            Some(idx) => (&trimmed[..idx], &trimmed[idx + 1..]),
            None => ("", trimmed),
        };
        if name.is_empty() {
            return None;
        }
        let dir = self.find_directory(dir_path)?;
        dir.entries.iter().find_map(|e| match e {
            DirectoryEntry::File(f) if f.name == name => Some(f),
            _ => None,
        })
    }

    pub fn find_file_mut(&mut self, path: &str) -> Option<&mut FileEntry> {
        let trimmed = path.trim_matches('/');
        let (dir_path, name) = match trimmed.rfind('/') {
            Some(idx) => (&trimmed[..idx], &trimmed[idx + 1..]),
            None => ("", trimmed),
        };
        let dir = self.find_directory_mut(dir_path)?;
        dir.entries.iter_mut().find_map(|entry| match entry {
            DirectoryEntry::File(file) if file.name == name => Some(file),
            _ => None,
        })
    }
}

fn count_entries(dir: &Directory, files: &mut u64, dirs: &mut u64) {
    for entry in &dir.entries {
        match entry {
            DirectoryEntry::File(_) => *files += 1,
            DirectoryEntry::Directory(d) => {
                *dirs += 1;
                count_entries(d, files, dirs);
            }
        }
    }
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
    Truncated,
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
            IndexError::Truncated => write!(f, "XML 意外结束"),
        }
    }
}

impl Index {
    /// 解析 LTFS index XML 文本。
    pub fn parse_xml(xml: &str) -> Result<Index, IndexError> {
        let mut reader = Reader::from_str(xml);
        // 保留 XML entity 两侧属于文件名/卷名的空格；嵌套字段解析器会按
        // 元素边界累计 Text/GeneralRef，元素间缩进不会写入字段。
        reader.config_mut().trim_text(false);

        let mut root_seen = false;
        let mut version = None;
        let mut creator = None;
        let mut volume_uuid = None;
        let mut generation = None;
        let mut update_time = None;
        let mut allow_policy_update = None;
        let mut volume_lock_state = None;
        let mut highest_file_uid = None;
        let mut self_partition = None;
        let mut self_block = None;
        let mut prev_partition = None;
        let mut prev_block = None;
        let mut root = None;

        let mut elem = String::new();
        let mut section = Section::Root;
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = element_name(e.name());
                    match name.as_str() {
                        "ltfsindex" => {
                            root_seen = true;
                            version = attr_value(&e, "version");
                        }
                        "location" if section == Section::Root => section = Section::Location,
                        "previousgenerationlocation" if section == Section::Root => {
                            section = Section::PrevLocation
                        }
                        "directory" if section == Section::Root => {
                            root = Some(parse_directory(&mut reader, &mut buf)?);
                        }
                        _ => {}
                    }
                    elem = name;
                }
                Ok(Event::Empty(e)) => {
                    let name = element_name(e.name());
                    // 自闭合的空根目录（测试 fixture 中的 <directory name="/"/>）。
                    if name == "directory" && section == Section::Root {
                        root = Some(Directory {
                            name: attr_value(&e, "name").unwrap_or_default(),
                            ..Default::default()
                        });
                    }
                    elem = name;
                }
                Ok(Event::Text(t)) => {
                    let text = unescape_text(&t)?;
                    if text.is_empty() {
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
        Ok(Index {
            version: version.ok_or(IndexError::MissingVersion)?,
            creator: creator.ok_or(IndexError::MissingField("creator"))?,
            volume_uuid: volume_uuid.ok_or(IndexError::MissingField("volumeuuid"))?,
            generation: generation.ok_or(IndexError::MissingField("generationnumber"))?,
            update_time: update_time.ok_or(IndexError::MissingField("updatetime"))?,
            self_location: TapePos {
                partition: self_partition.ok_or(IndexError::MissingField("location/partition"))?,
                startblock: self_block.ok_or(IndexError::MissingField("location/startblock"))?,
            },
            previous_location: match (prev_partition, prev_block) {
                (Some(partition), Some(startblock)) => Some(TapePos {
                    partition,
                    startblock,
                }),
                _ => None,
            },
            allow_policy_update: allow_policy_update
                .ok_or(IndexError::MissingField("allowpolicyupdate"))?,
            volume_lock_state,
            highest_file_uid,
            root: root.ok_or(IndexError::MissingField("directory"))?,
        })
    }

    /// 序列化为标准 LTFS index XML（与 OpenLTFS 写出的字段一致）。
    pub fn to_xml(&self) -> String {
        let mut s = String::new();
        s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        s.push_str(&format!(
            "<ltfsindex version=\"{}\">\n",
            escape_text(&self.version)
        ));
        s.push_str(&format!(
            "<creator>{}</creator>\n",
            escape_text(&self.creator)
        ));
        s.push_str(&format!(
            "<volumeuuid>{}</volumeuuid>\n",
            escape_text(&self.volume_uuid)
        ));
        s.push_str(&format!(
            "<generationnumber>{}</generationnumber>\n",
            self.generation
        ));
        s.push_str(&format!(
            "<updatetime>{}</updatetime>\n",
            escape_text(&self.update_time)
        ));
        write_tape_pos(&mut s, "location", &self.self_location);
        if let Some(prev) = &self.previous_location {
            write_tape_pos(&mut s, "previousgenerationlocation", prev);
        }
        s.push_str(&format!(
            "<allowpolicyupdate>{}</allowpolicyupdate>\n",
            self.allow_policy_update
        ));
        if let Some(uid) = self.highest_file_uid {
            s.push_str(&format!("<highestfileuid>{uid}</highestfileuid>\n"));
        }
        if let Some(state) = &self.volume_lock_state {
            s.push_str(&format!(
                "<volumelockstate>{}</volumelockstate>\n",
                escape_text(state)
            ));
        }
        write_directory(&mut s, &self.root);
        s.push_str("</ltfsindex>\n");
        s
    }
}

fn write_tape_pos(s: &mut String, tag: &str, pos: &TapePos) {
    s.push_str(&format!("<{tag}>\n"));
    s.push_str(&format!(
        "<partition>{}</partition>\n",
        partition_name(pos.partition)
    ));
    s.push_str(&format!("<startblock>{}</startblock>\n", pos.startblock));
    s.push_str(&format!("</{tag}>\n"));
}

fn write_directory(s: &mut String, dir: &Directory) {
    s.push_str("<directory>\n");
    s.push_str(&format!("<name>{}</name>\n", escape_text(&dir.name)));
    s.push_str(&format!("<readonly>{}</readonly>\n", dir.readonly));
    write_times(s, &dir.times);
    s.push_str(&format!("<fileuid>{}</fileuid>\n", dir.fileuid));
    write_extended_attributes(s, &dir.extended_attributes);
    s.push_str("<contents>\n");
    for entry in &dir.entries {
        match entry {
            DirectoryEntry::File(f) => write_file(s, f),
            DirectoryEntry::Directory(d) => write_directory(s, d),
        }
    }
    s.push_str("</contents>\n");
    s.push_str("</directory>\n");
}

fn write_file(s: &mut String, file: &FileEntry) {
    s.push_str("<file>\n");
    s.push_str(&format!("<name>{}</name>\n", escape_text(&file.name)));
    s.push_str(&format!("<length>{}</length>\n", file.length));
    s.push_str(&format!("<readonly>{}</readonly>\n", file.readonly));
    write_times(s, &file.times);
    s.push_str(&format!("<fileuid>{}</fileuid>\n", file.fileuid));
    write_extended_attributes(s, &file.extended_attributes);
    if let Some(target) = &file.symlink_target {
        s.push_str(&format!("<symlink>{}</symlink>\n", escape_text(target)));
    } else if !file.extents.is_empty() {
        s.push_str("<extentinfo>\n");
        for extent in &file.extents {
            s.push_str("<extent>\n");
            s.push_str(&format!(
                "<fileoffset>{}</fileoffset>\n",
                extent.file_offset
            ));
            s.push_str(&format!(
                "<partition>{}</partition>\n",
                partition_name(extent.partition)
            ));
            s.push_str(&format!(
                "<startblock>{}</startblock>\n",
                extent.start_block
            ));
            // OpenLTFS 解析时 byteoffset 是必需字段；LTFSCopyGUI 写入恒 0。
            s.push_str("<byteoffset>0</byteoffset>\n");
            s.push_str(&format!("<bytecount>{}</bytecount>\n", extent.byte_count));
            s.push_str("</extent>\n");
        }
        s.push_str("</extentinfo>\n");
    }
    s.push_str("</file>\n");
}

fn write_extended_attributes(s: &mut String, attrs: &[ExtendedAttribute]) {
    if attrs.is_empty() {
        return;
    }
    s.push_str("<extendedattributes>\n");
    for attr in attrs {
        s.push_str("<xattr>\n");
        s.push_str(&format!("<key>{}</key>\n", escape_text(&attr.key)));
        match &attr.value_type {
            Some(kind) => s.push_str(&format!(
                "<value type=\"{}\">{}</value>\n",
                escape_text(kind),
                escape_text(&attr.value)
            )),
            None => s.push_str(&format!("<value>{}</value>\n", escape_text(&attr.value))),
        }
        s.push_str("</xattr>\n");
    }
    s.push_str("</extendedattributes>\n");
}

fn write_times(s: &mut String, times: &FileTimes) {
    s.push_str(&format!(
        "<creationtime>{}</creationtime>\n",
        escape_text(times.creation_time.as_deref().unwrap_or(""))
    ));
    s.push_str(&format!(
        "<changetime>{}</changetime>\n",
        escape_text(times.change_time.as_deref().unwrap_or(""))
    ));
    s.push_str(&format!(
        "<modifytime>{}</modifytime>\n",
        escape_text(times.modify_time.as_deref().unwrap_or(""))
    ));
    s.push_str(&format!(
        "<accesstime>{}</accesstime>\n",
        escape_text(times.access_time.as_deref().unwrap_or(""))
    ));
    s.push_str(&format!(
        "<backuptime>{}</backuptime>\n",
        escape_text(times.backup_time.as_deref().unwrap_or(""))
    ));
}

fn partition_name(partition: u8) -> String {
    match partition {
        0 => "a".into(),
        1 => "b".into(),
        // 规范只定义 a/b；异常值按数字输出便于调试。
        other => other.to_string(),
    }
}

fn escape_text(text: &str) -> String {
    quick_xml::escape::escape(text).into_owned()
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Root,
    Location,
    PrevLocation,
}

/// 解析一个 `<directory>` 元素（reader 位于其 Start 事件之后），
/// 返回后 reader 已越过对应的 End 事件。
fn parse_directory(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Result<Directory, IndexError> {
    let mut dir = Directory::default();
    let mut pending = Pending::None;
    let mut field_text = String::new();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => {
                let name = element_name(e.name());
                match name.as_str() {
                    "contents" => {}
                    "file" => {
                        dir.entries
                            .push(DirectoryEntry::File(parse_file(reader, buf)?));
                        pending = Pending::None;
                    }
                    "directory" => {
                        dir.entries
                            .push(DirectoryEntry::Directory(parse_directory(reader, buf)?));
                        pending = Pending::None;
                    }
                    "extendedattributes" => {
                        dir.extended_attributes = parse_extended_attributes(reader, buf)?;
                        pending = Pending::None;
                    }
                    "name" => begin_field(&mut pending, &mut field_text, Pending::Name),
                    "fileuid" => begin_field(&mut pending, &mut field_text, Pending::FileUid),
                    "readonly" => begin_field(&mut pending, &mut field_text, Pending::Readonly),
                    "creationtime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Creation),
                    ),
                    "changetime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Change),
                    ),
                    "modifytime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Modify),
                    ),
                    "accesstime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Access),
                    ),
                    "backuptime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Backup),
                    ),
                    _ => {}
                }
            }
            Ok(Event::Empty(_)) => {
                pending = Pending::None;
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
                if element_name(e.name()) == "directory" {
                    return Ok(dir);
                }
                if field_text.is_empty() {
                    pending = Pending::None;
                } else {
                    apply_pending(&mut pending, &field_text, |field, value| {
                        match field {
                            DirField::Name => dir.name = value,
                            DirField::FileUid => dir.fileuid = parse_u64(&value, "fileuid")?,
                            DirField::Readonly => dir.readonly = parse_bool(&value, "readonly")?,
                            DirField::Time(f) => set_time(&mut dir.times, f, value),
                            DirField::Length => unreachable!("目录没有 length"),
                            DirField::SymlinkTarget => unreachable!("目录没有 symlink"),
                        }
                        Ok(())
                    })?;
                }
                field_text.clear();
            }
            Ok(Event::Eof) => return Err(IndexError::Truncated),
            Err(e) => return Err(IndexError::Xml(e.to_string())),
            _ => {}
        }
    }
}

/// 解析一个 `<file>` 元素。
fn parse_file(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Result<FileEntry, IndexError> {
    let mut file = FileEntry::default();
    let mut pending = Pending::None;
    let mut field_text = String::new();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => {
                let name = element_name(e.name());
                match name.as_str() {
                    "extentinfo" => {}
                    "extent" => {
                        file.extents.push(parse_extent(reader, buf)?);
                        pending = Pending::None;
                    }
                    "extendedattributes" => {
                        file.extended_attributes = parse_extended_attributes(reader, buf)?;
                        pending = Pending::None;
                    }
                    "name" => begin_field(&mut pending, &mut field_text, Pending::Name),
                    "fileuid" => begin_field(&mut pending, &mut field_text, Pending::FileUid),
                    "length" => begin_field(&mut pending, &mut field_text, Pending::Length),
                    "readonly" => begin_field(&mut pending, &mut field_text, Pending::Readonly),
                    "creationtime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Creation),
                    ),
                    "changetime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Change),
                    ),
                    "modifytime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Modify),
                    ),
                    "accesstime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Access),
                    ),
                    "backuptime" => begin_field(
                        &mut pending,
                        &mut field_text,
                        Pending::Time(TimeField::Backup),
                    ),
                    "symlink" => begin_field(&mut pending, &mut field_text, Pending::SymlinkTarget),
                    _ => {}
                }
            }
            Ok(Event::Empty(_)) => {
                pending = Pending::None;
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
                if element_name(e.name()) == "file" {
                    return Ok(file);
                }
                if field_text.is_empty() {
                    pending = Pending::None;
                } else {
                    apply_pending(&mut pending, &field_text, |field, value| {
                        match field {
                            DirField::Name => file.name = value,
                            DirField::FileUid => file.fileuid = parse_u64(&value, "fileuid")?,
                            DirField::Readonly => file.readonly = parse_bool(&value, "readonly")?,
                            DirField::Length => file.length = parse_u64(&value, "length")?,
                            DirField::Time(f) => set_time(&mut file.times, f, value),
                            DirField::SymlinkTarget => file.symlink_target = Some(value),
                        }
                        Ok(())
                    })?;
                }
                field_text.clear();
            }
            Ok(Event::Eof) => return Err(IndexError::Truncated),
            Err(e) => return Err(IndexError::Xml(e.to_string())),
            _ => {}
        }
    }
}

fn parse_extended_attributes(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
) -> Result<Vec<ExtendedAttribute>, IndexError> {
    let mut attrs = Vec::new();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) if element_name(e.name()) == "xattr" => {
                attrs.push(parse_xattr(reader, buf)?);
            }
            Ok(Event::End(e)) if element_name(e.name()) == "extendedattributes" => {
                return Ok(attrs);
            }
            Ok(Event::Eof) => return Err(IndexError::Truncated),
            Err(e) => return Err(IndexError::Xml(e.to_string())),
            _ => {}
        }
    }
}

fn parse_xattr(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
) -> Result<ExtendedAttribute, IndexError> {
    let mut attr = ExtendedAttribute::default();
    let mut field = None;
    let mut text = String::new();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match element_name(e.name()).as_str() {
                "key" => {
                    field = Some("key");
                    text.clear();
                }
                "value" => {
                    attr.value_type = attr_value(&e, "type");
                    field = Some("value");
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Text(t)) => text.push_str(&unescape_text(&t)?),
            Ok(Event::GeneralRef(r)) => text.push_str(&decode_general_ref(&r)?),
            Ok(Event::End(e)) => match element_name(e.name()).as_str() {
                "key" => {
                    attr.key = std::mem::take(&mut text);
                    field = None;
                }
                "value" => {
                    attr.value = std::mem::take(&mut text);
                    field = None;
                }
                "xattr" => return Ok(attr),
                _ => {}
            },
            Ok(Event::Eof) => return Err(IndexError::Truncated),
            Err(e) => return Err(IndexError::Xml(e.to_string())),
            _ => {
                let _ = field;
            }
        }
    }
}

/// 解析一个 `<extent>` 元素。
fn parse_extent(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Result<Extent, IndexError> {
    let mut extent = Extent::default();
    let mut pending = Pending::None;
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                pending = match element_name(e.name()).as_str() {
                    "fileoffset" => Pending::FileOffset,
                    "startblock" => Pending::StartBlock,
                    "bytecount" => Pending::ByteCount,
                    "partition" => Pending::Partition,
                    _ => Pending::None,
                };
            }
            Ok(Event::Text(t)) => {
                let text = unescape_text(&t)?;
                if !text.is_empty() {
                    match std::mem::replace(&mut pending, Pending::None) {
                        Pending::FileOffset => extent.file_offset = parse_u64(&text, "fileoffset")?,
                        Pending::StartBlock => extent.start_block = parse_u64(&text, "startblock")?,
                        Pending::ByteCount => extent.byte_count = parse_u64(&text, "bytecount")?,
                        Pending::Partition => {
                            extent.partition = parse_partition(&text, "partition")?
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                if element_name(e.name()).as_str() == "extent" {
                    return Ok(extent);
                }
                pending = Pending::None;
            }
            Ok(Event::Eof) => return Err(IndexError::Truncated),
            Err(e) => return Err(IndexError::Xml(e.to_string())),
            _ => {}
        }
    }
}

/// 等待文本填充的字段。
#[derive(Clone, Copy)]
enum Pending {
    None,
    Name,
    FileUid,
    Readonly,
    Length,
    FileOffset,
    StartBlock,
    ByteCount,
    Partition,
    SymlinkTarget,
    Time(TimeField),
}

#[derive(Clone, Copy)]
enum TimeField {
    Creation,
    Change,
    Modify,
    Access,
    Backup,
}

/// 目录/文件条目可填充的字段（Text 事件到来时应用）。
enum DirField {
    Name,
    FileUid,
    Readonly,
    Length,
    Time(TimeField),
    SymlinkTarget,
}

fn begin_field(pending: &mut Pending, text: &mut String, field: Pending) {
    *pending = field;
    text.clear();
}

fn decode_general_ref(r: &quick_xml::events::BytesRef<'_>) -> Result<String, IndexError> {
    let name = r.decode().map_err(|e| IndexError::Xml(e.to_string()))?;
    let encoded = format!("&{name};");
    quick_xml::escape::unescape(&encoded)
        .map(|s| s.into_owned())
        .map_err(|e| IndexError::Xml(e.to_string()))
}

fn apply_pending(
    pending: &mut Pending,
    text: &str,
    apply: impl FnOnce(DirField, String) -> Result<(), IndexError>,
) -> Result<(), IndexError> {
    let field = match std::mem::replace(pending, Pending::None) {
        Pending::Name => Some(DirField::Name),
        Pending::FileUid => Some(DirField::FileUid),
        Pending::Readonly => Some(DirField::Readonly),
        Pending::Length => Some(DirField::Length),
        Pending::SymlinkTarget => Some(DirField::SymlinkTarget),
        Pending::Time(f) => Some(DirField::Time(f)),
        _ => None,
    };
    if let Some(field) = field {
        apply(field, text.to_string())?;
    }
    Ok(())
}

fn set_time(times: &mut FileTimes, field: TimeField, value: String) {
    match field {
        TimeField::Creation => times.creation_time = Some(value),
        TimeField::Change => times.change_time = Some(value),
        TimeField::Modify => times.modify_time = Some(value),
        TimeField::Access => times.access_time = Some(value),
        TimeField::Backup => times.backup_time = Some(value),
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
  <highestfileuid>5</highestfileuid>
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
            <startblock>7</startblock>
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
      <file>
        <name>link</name>
        <length>0</length>
        <readonly>false</readonly>
        <creationtime>2026-06-18T11:28:00.0000000+00:00</creationtime>
        <changetime>2026-06-18T11:28:00.0000000+00:00</changetime>
        <fileuid>5</fileuid>
        <symlink>/notes.txt</symlink>
      </file>
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
    fn parse_index_xml_with_tree() {
        let idx = Index::parse_xml(INDEX_XML).unwrap();
        assert_eq!(idx.version, "2.4.0");
        assert_eq!(idx.volume_name(), Some("/"));
        assert_eq!(idx.generation, 2);
        assert_eq!(
            idx.self_location,
            TapePos {
                partition: 0,
                startblock: 5
            }
        );
        assert_eq!(
            idx.previous_location,
            Some(TapePos {
                partition: 0,
                startblock: 4
            })
        );
        assert!(!idx.allow_policy_update);
        assert_eq!(idx.volume_lock_state.as_deref(), Some("unlocked"));
        assert_eq!(idx.highest_file_uid, Some(5));
        assert_eq!(idx.file_count(), 3); // notes.txt + inner.bin + symlink
        assert_eq!(idx.directory_count(), 1); // sub

        assert_eq!(idx.root.entries.len(), 3);
        let DirectoryEntry::File(notes) = &idx.root.entries[0] else {
            panic!("第一个条目应为文件");
        };
        assert_eq!(notes.name, "notes.txt");
        assert_eq!(notes.length, 1024);
        assert_eq!(notes.extents.len(), 1);
        assert_eq!(
            notes.extents[0],
            Extent {
                file_offset: 0,
                partition: 1,
                start_block: 7,
                byte_count: 1024
            }
        );

        let DirectoryEntry::Directory(sub) = &idx.root.entries[1] else {
            panic!("第二个条目应为目录");
        };
        assert_eq!(sub.name, "sub");
        assert_eq!(sub.entries.len(), 1);

        let DirectoryEntry::File(link) = &idx.root.entries[2] else {
            panic!("第三个条目应为符号链接（以 file 形式存储）");
        };
        assert_eq!(link.name, "link");
        assert_eq!(link.symlink_target.as_deref(), Some("/notes.txt"));
    }

    #[test]
    fn find_directory_by_path() {
        let idx = Index::parse_xml(INDEX_XML).unwrap();
        let sub = idx.find_directory("/sub").unwrap();
        assert_eq!(sub.name, "sub");
        assert!(idx.find_directory("/nope").is_none());
        assert_eq!(idx.find_directory("/").unwrap().name, "/");
    }

    #[test]
    fn find_file_by_path() {
        let idx = Index::parse_xml(INDEX_XML).unwrap();
        let f = idx.find_file("/notes.txt").unwrap();
        assert_eq!(f.name, "notes.txt");
        assert_eq!(f.length, 1024);
        let inner = idx.find_file("/sub/inner.bin").unwrap();
        assert_eq!(inner.name, "inner.bin");
        assert_eq!(inner.length, 2048);
        assert!(idx.find_file("/nope").is_none());
        assert!(idx.find_file("/sub").is_none());
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
    fn parse_real_mkltfs_index() {
        let idx = Index::parse_xml(REAL_INDEX_XML).unwrap();
        assert_eq!(idx.volume_name(), Some("tapecpy M2 test"));
        assert_eq!(
            idx.creator,
            "IBM LTFS 2.4.8.4 (Prelim) - Linux - mkltfs - Format"
        );
        assert_eq!(idx.volume_uuid, "0cc710d4-bc2f-4c54-823e-a44413987f5d");
        assert_eq!(idx.generation, 1);
        assert_eq!(
            idx.self_location,
            TapePos {
                partition: 0,
                startblock: 5
            }
        );
        assert_eq!(
            idx.previous_location,
            Some(TapePos {
                partition: 1,
                startblock: 5
            })
        );
        assert!(idx.allow_policy_update);
        assert_eq!(idx.highest_file_uid, Some(1));
        assert_eq!(idx.volume_lock_state.as_deref(), Some("unlocked"));
        assert_eq!(idx.file_count(), 0);
        assert_eq!(idx.directory_count(), 0);
        assert!(idx.root.entries.is_empty());
    }

    #[test]
    fn to_xml_round_trip() {
        let idx = Index::parse_xml(INDEX_XML).unwrap();
        let xml = idx.to_xml();
        let reparsed = Index::parse_xml(&xml).unwrap();
        assert_eq!(reparsed, idx);
    }

    #[test]
    fn to_xml_real_index_round_trip() {
        let idx = Index::parse_xml(REAL_INDEX_XML).unwrap();
        let xml = idx.to_xml();
        let reparsed = Index::parse_xml(&xml).unwrap();
        assert_eq!(reparsed, idx);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    }

    #[test]
    fn round_trip_name_with_xml_entities() {
        let mut idx = Index::parse_xml(INDEX_XML).unwrap();
        let DirectoryEntry::File(file) = &mut idx.root.entries[0] else {
            panic!("第一个条目应为文件");
        };
        file.name = "xml&less<tag>\"quote\".bin".into();

        let xml = idx.to_xml();
        assert!(xml.contains("xml&amp;less&lt;tag&gt;&quot;quote&quot;.bin"));
        let reparsed = Index::parse_xml(&xml).unwrap();
        assert_eq!(
            reparsed.root.entries[0].name(),
            "xml&less<tag>\"quote\".bin"
        );
    }

    #[test]
    fn extended_attributes_round_trip_without_losing_type_or_entities() {
        let mut idx = Index::parse_xml(INDEX_XML).unwrap();
        let expected = vec![
            ExtendedAttribute {
                key: "ltfs.hash.sha256sum".into(),
                value: "abc123".into(),
                value_type: None,
            },
            ExtendedAttribute {
                key: "vendor&key".into(),
                value: "binary+value==".into(),
                value_type: Some("base64".into()),
            },
        ];
        idx.find_file_mut("/notes.txt").unwrap().extended_attributes = expected.clone();

        let xml = idx.to_xml();
        assert!(xml.contains("<key>vendor&amp;key</key>"));
        assert!(xml.contains("<value type=\"base64\">binary+value==</value>"));
        let reparsed = Index::parse_xml(&xml).unwrap();
        assert_eq!(
            reparsed
                .find_file("/notes.txt")
                .unwrap()
                .extended_attributes,
            expected
        );
    }
}
