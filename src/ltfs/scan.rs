//! 从磁带记录流中定位最新 LTFS index 的纯逻辑。
//!
//! index 分区在 label 之后的布局是：
//!
//! ```text
//! [index XML 记录...][FileMark][index XML 记录...][FileMark] ... EOD
//! ```
//!
//! 一个 index 文件可以跨多个记录，以 FileMark 结束。本模块按 FileMark
//! 分组尝试解析，返回最后一个能解析成功的 index XML；解析失败的分组
//! （例如损坏的旧 index）会被跳过，而不是中断扫描。

use super::index::Index;

/// 扫描输入中的一条磁带记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanRecord {
    Data(Vec<u8>),
    Filemark,
    Eod,
}

/// 返回记录流中最后一个能解析为 LTFS index 的 XML 文本；没有则为 None。
pub fn find_latest_index<I>(records: I) -> Option<String>
where
    I: IntoIterator<Item = ScanRecord>,
{
    let mut latest = None;
    let mut current = Vec::new();
    let mut saw_data = false;

    for rec in records {
        match rec {
            ScanRecord::Data(bytes) => {
                current.extend_from_slice(&bytes);
                saw_data = true;
            }
            ScanRecord::Filemark | ScanRecord::Eod => {
                if saw_data
                    && let Ok(text) = std::str::from_utf8(&current)
                    && Index::parse_xml(text).is_ok()
                {
                    latest = Some(text.to_string());
                }
                current.clear();
                saw_data = false;
                if matches!(rec, ScanRecord::Eod) {
                    break;
                }
            }
        }
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_xml(generation: u64) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ltfsindex version="2.4.0">
  <creator>test</creator>
  <volumeuuid>c72c9917-cf2e-4873-8b64-9ac05b2f324c</volumeuuid>
  <generationnumber>{generation}</generationnumber>
  <updatetime>2026-06-18T11:28:00.0000000+00:00</updatetime>
  <location>
    <partition>0</partition>
    <startblock>{}</startblock>
  </location>
  <allowpolicyupdate>false</allowpolicyupdate>
  <highestfileuid>1</highestfileuid>
  <directory name="/"/>
</ltfsindex>"#,
            4 + generation
        )
    }

    #[test]
    fn picks_latest_parseable_index() {
        let gen1 = index_xml(1);
        let gen2 = index_xml(2);
        let gen2_head = &gen2.as_bytes()[..gen2.len() / 2];
        let gen2_tail = &gen2.as_bytes()[gen2.len() / 2..];

        let records = vec![
            ScanRecord::Filemark,
            ScanRecord::Data(gen1.as_bytes().to_vec()),
            ScanRecord::Filemark,
            ScanRecord::Data(gen2_head.to_vec()),
            ScanRecord::Data(gen2_tail.to_vec()),
            ScanRecord::Filemark,
            ScanRecord::Eod,
        ];
        let latest = find_latest_index(records).unwrap();
        let idx = Index::parse_xml(&latest).unwrap();
        assert_eq!(idx.generation, 2);
    }

    #[test]
    fn skips_corrupted_index_and_keeps_older_one() {
        let gen1 = index_xml(1);
        let corrupted = b"<ltfsindex version=\"2.4.0\"><garbage";
        let records = vec![
            ScanRecord::Filemark,
            ScanRecord::Data(gen1.as_bytes().to_vec()),
            ScanRecord::Filemark,
            ScanRecord::Data(corrupted.to_vec()),
            ScanRecord::Filemark,
            ScanRecord::Eod,
        ];
        let latest = find_latest_index(records).unwrap();
        let idx = Index::parse_xml(&latest).unwrap();
        assert_eq!(idx.generation, 1);
    }

    #[test]
    fn no_index_returns_none() {
        let records = vec![
            ScanRecord::Data(b"random non-xml data".to_vec()),
            ScanRecord::Filemark,
            ScanRecord::Eod,
        ];
        assert_eq!(find_latest_index(records), None);
    }
}
