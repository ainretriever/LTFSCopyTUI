//! LTFS 格式层（与设备 I/O 分离的纯数据逻辑）。
//!
//! 本层不访问磁带设备，也不依赖 TUI/CLI：
//! - `label`：ANSI VOL1 label 与 LTFS XML label 的解析；
//! - `index`：LTFS XML index 的解析与摘要；
//! - `scan`：从磁带记录流中定位最新 index 的纯逻辑。
//!
//! 设备层通过 record 流与本层交互：本层只消费字节，不关心这些字节来自
//! 真实磁带还是测试 fixture。

pub mod index;
pub mod label;
pub mod mam;
pub mod scan;

use quick_xml::Reader;
use quick_xml::events::Event;

pub(crate) fn validate_xml_document(xml: &str, expected_root: &[u8]) -> Result<(), String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut depth = 0usize;
    let mut root_seen = false;
    let mut root_closed = false;
    let mut declaration_seen = false;

    loop {
        match reader.read_event() {
            Ok(Event::Decl(_)) => {
                if declaration_seen || root_seen {
                    return Err("XML declaration must appear once before the root element".into());
                }
                declaration_seen = true;
            }
            Ok(Event::DocType(_)) => {
                return Err("DOCTYPE is not permitted in LTFS metadata XML".into());
            }
            Ok(Event::Start(element)) => {
                validate_xml_attributes(&element)?;
                if depth == 0 {
                    if root_seen {
                        return Err("XML contains more than one root element".into());
                    }
                    if element.name().as_ref() != expected_root {
                        return Err(format!(
                            "XML root element is <{}>, expected <{}>",
                            String::from_utf8_lossy(element.name().as_ref()),
                            String::from_utf8_lossy(expected_root)
                        ));
                    }
                    root_seen = true;
                }
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| "XML nesting depth overflow".to_string())?;
            }
            Ok(Event::Empty(element)) => {
                validate_xml_attributes(&element)?;
                if depth == 0 {
                    if root_seen {
                        return Err("XML contains more than one root element".into());
                    }
                    if element.name().as_ref() != expected_root {
                        return Err(format!(
                            "XML root element is <{}>, expected <{}>",
                            String::from_utf8_lossy(element.name().as_ref()),
                            String::from_utf8_lossy(expected_root)
                        ));
                    }
                    root_seen = true;
                    root_closed = true;
                }
            }
            Ok(Event::End(_)) => {
                if depth == 0 {
                    return Err("XML contains an unmatched closing element".into());
                }
                depth -= 1;
                if depth == 0 {
                    root_closed = true;
                }
            }
            Ok(Event::Text(text)) if depth == 0 => {
                if !text
                    .as_ref()
                    .iter()
                    .all(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
                {
                    return Err("XML contains text outside the root element".into());
                }
            }
            Ok(Event::CData(_)) | Ok(Event::GeneralRef(_)) if depth == 0 => {
                return Err("XML contains character data outside the root element".into());
            }
            Ok(Event::Eof) => {
                if !root_seen {
                    return Err("XML has no root element".into());
                }
                if depth != 0 || !root_closed {
                    return Err("XML ended before the root element was closed".into());
                }
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
            _ => {}
        }
    }
}

fn validate_xml_attributes(element: &quick_xml::events::BytesStart<'_>) -> Result<(), String> {
    for attribute in element.attributes() {
        attribute.map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod xml_tests {
    use super::validate_xml_document;

    #[test]
    fn validates_complete_document_and_rejects_malformed_document() {
        validate_xml_document(
            "<?xml version=\"1.0\"?><ltfsindex><directory/></ltfsindex>\n",
            b"ltfsindex",
        )
        .unwrap();
        assert!(validate_xml_document("<ltfsindex><directory/>", b"ltfsindex").is_err());
        assert!(validate_xml_document("<ltfsindex/><ltfsindex/>", b"ltfsindex").is_err());
        assert!(validate_xml_document("<!DOCTYPE ltfsindex><ltfsindex/>", b"ltfsindex").is_err());
        assert!(validate_xml_document("<ltfsindex><a></ltfsindex>", b"ltfsindex").is_err());
    }
}
