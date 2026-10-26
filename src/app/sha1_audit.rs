//! Read-only existing-volume audit; hashes and indexes on tape are never updated.
use super::*;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, serde::Serialize)]
pub struct Sha1AuditEvent {
    pub path: String,
    pub length: u64,
    pub files_total: usize,
    pub actual_sha1: Option<String>,
    pub bytes_read: u64,
    pub expected_sha1: String,
    /// started, reading, passed, mismatch, skipped_previous_pass, read_failed.
    pub status: &'static str,
    pub error: Option<String>,
    pub seconds: f64,
}

fn collect<'a>(
    dir: &'a crate::ltfs::index::Directory,
    prefix: &str,
    files: &mut Vec<(String, &'a FileEntry, String)>,
) -> Result<(), String> {
    for entry in &dir.entries {
        let path = format!("{prefix}/{}", entry.name());
        match entry {
            DirectoryEntry::Directory(child) => collect(child, &path, files)?,
            DirectoryEntry::File(file) => {
                if file.symlink_target.is_some() {
                    return Err(format!("SHA1 audit不支持符号链接: {path}"));
                }
                let hashes: Vec<_> = file
                    .extended_attributes
                    .iter()
                    .filter(|a| a.key.eq_ignore_ascii_case("ltfs.hash.sha1sum"))
                    .collect();
                if hashes.len() != 1 {
                    return Err(format!("{path} 缺少唯一SHA1摘要"));
                }
                let hash = hashes[0].value.trim().to_ascii_lowercase();
                if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("{path} SHA1摘要格式无效"));
                }
                let mut offset = 0u64;
                for extent in &file.extents {
                    if extent.file_offset != offset {
                        return Err(format!("{path} 稀疏/乱序extent尚不支持SHA1审计"));
                    }
                    offset = offset
                        .checked_add(extent.byte_count)
                        .ok_or("extent长度溢出")?;
                }
                if offset != file.length {
                    return Err(format!("{path} extent长度与文件长度不一致"));
                }
                files.push((path, file, hash));
            }
        }
    }
    Ok(())
}

/// Prior successes are checked against current paths, lengths and SHA1 values.
/// One lease/session/cursor and one SHA worker cover the entire ordered batch.
pub fn audit_sha1_volume(
    drive: &TapeDrive,
    previous: &BTreeMap<String, (u64, String)>,
    cancellation: &CancellationToken,
    observer: &mut dyn FnMut(&Sha1AuditEvent),
) -> Result<(), String> {
    let _lease = device::lease::DeviceLease::try_acquire(
        &drive.serial,
        device::lease::LeaseOwner::new("audit", "sha1-volume-read-only"),
    )?;
    let mut session = TapeSession::open(&drive.sg_path).map_err(|e| e.to_string())?;
    let volume = inspect_volume_session(&mut session).map_err(|e| e.to_string())?;
    let index = volume.index.ok_or("没有可读LTFS索引")?;
    let files = plan_files(&index, previous)?;
    audit_files(&mut session, files, previous, cancellation, observer)
}

fn plan_files<'a>(
    index: &'a Index,
    previous: &BTreeMap<String, (u64, String)>,
) -> Result<Vec<(String, &'a FileEntry, String)>, String> {
    let mut files = Vec::new();
    collect(&index.root, "", &mut files)?;
    let mut unique = BTreeSet::new();
    for (path, _, _) in &files {
        if !unique.insert(path) {
            return Err(format!("重复文件路径: {path}"));
        }
    }
    // Whole files in physical-start order; extents remain in logical file order.
    files.sort_by_key(|(_, f, _)| {
        f.extents
            .first()
            .map(|e| (e.partition, e.start_block))
            .unwrap_or((0, 0))
    });
    for (path, (length, hash)) in previous {
        if !files
            .iter()
            .any(|(p, f, h)| p == path && f.length == *length && h.eq_ignore_ascii_case(hash))
        {
            return Err(format!("旧通过记录与当前索引不一致: {path}"));
        }
    }
    Ok(files)
}

fn audit_files(
    tape: &mut impl ExtentTape,
    files: Vec<(String, &FileEntry, String)>,
    previous: &BTreeMap<String, (u64, String)>,
    cancellation: &CancellationToken,
    observer: &mut dyn FnMut(&Sha1AuditEvent),
) -> Result<(), String> {
    let mut pipeline = verify_pipeline::VerifyPipeline::spawn_sha1(cancellation.clone())?;
    let mut cursor = ExtentReadCursor::default();
    let mut mismatches = 0;
    let files_total = files.len();
    for (path, file, hash) in files {
        if cancellation.is_cancelled() {
            return Err("[cancelled]SHA1审计已取消".into());
        }
        let mut event = Sha1AuditEvent {
            path: path.clone(),
            length: file.length,
            files_total,
            actual_sha1: None,
            bytes_read: 0,
            expected_sha1: hash.clone(),
            status: "started",
            error: None,
            seconds: 0.0,
        };
        if previous.contains_key(&path) {
            event.actual_sha1 = Some(hash.clone());
            event.status = "skipped_previous_pass";
            event.bytes_read = file.length;
            observer(&event);
            continue;
        }
        observer(&event);
        let start = Instant::now();
        let mut last = Instant::now();
        let result = verify_file_hash_with_progress(
            tape,
            file,
            (&path, &hash),
            &mut cursor,
            &mut pipeline,
            &mut |bytes| {
                event.bytes_read = bytes;
                if last.elapsed() >= Duration::from_secs(1) {
                    event.status = "reading";
                    event.seconds = start.elapsed().as_secs_f64();
                    observer(&event);
                    last = Instant::now();
                }
            },
        );
        event.seconds = start.elapsed().as_secs_f64();
        match result {
            Ok(()) => {
                event.actual_sha1 = Some(hash.clone());
                event.status = "passed";
                observer(&event);
            }
            Err(error) => {
                // Only full-data digest mismatch may continue; all I/O/cancel failures stop.
                let mismatch =
                    matches!(&error, verify_pipeline::VerifyError::DigestMismatch { .. });
                if let verify_pipeline::VerifyError::DigestMismatch { actual, .. } = &error {
                    event.actual_sha1 = Some(actual.clone());
                }
                event.status = if mismatch { "mismatch" } else { "read_failed" };
                event.error = Some(error.to_string());
                observer(&event);
                if !mismatch {
                    return Err(error.to_string());
                }
                // A mismatch terminates this worker: start another for the next file.
                mismatches += 1;
                pipeline = verify_pipeline::VerifyPipeline::spawn_sha1(cancellation.clone())?;
            }
        }
    }
    if mismatches > 0 {
        Err(format!("SHA1审计完成，{mismatches} 个文件摘要不匹配"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(hash: &str) -> FileEntry {
        FileEntry {
            name: "a".into(),
            length: 3,
            extents: vec![Extent {
                partition: 1,
                start_block: 10,
                byte_offset: 7,
                byte_count: 3,
                file_offset: 0,
            }],
            extended_attributes: vec![ExtendedAttribute {
                key: "ltfs.hash.sha1sum".into(),
                value: hash.into(),
                value_type: None,
            }],
            ..Default::default()
        }
    }
    struct FakeTape {
        position: u64,
        reads: usize,
        fail: bool,
    }
    impl ExtentTape for FakeTape {
        fn locate_extent(&mut self, _: u8, block: u64) -> Result<(), String> {
            self.position = block;
            Ok(())
        }
        fn read_extent_record(&mut self, mut buffer: Vec<u8>) -> Result<Vec<u8>, String> {
            self.reads += 1;
            if self.fail {
                return Err("injected device error: SHA-1 不匹配".into());
            }
            buffer.clear();
            buffer.extend_from_slice(b"abc");
            self.position += 1;
            Ok(buffer)
        }
    }
    #[test]
    fn mismatch_continues_but_read_error_with_similar_text_stops() {
        let mut bad = file(&"0".repeat(40));
        bad.extents[0].byte_offset = 0;
        let mut good = file("a9993e364706816aba3e25717850c26c9cd0d89d");
        good.extents[0].byte_offset = 0;
        good.extents[0].start_block = 11;
        for fail in [false, true] {
            let files = vec![
                ("/bad".into(), &bad, "0".repeat(40)),
                (
                    "/good".into(),
                    &good,
                    "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
                ),
            ];
            let mut tape = FakeTape {
                position: 0,
                reads: 0,
                fail,
            };
            let mut terminal = Vec::new();
            let result = audit_files(
                &mut tape,
                files,
                &BTreeMap::new(),
                &CancellationToken::default(),
                &mut |event| {
                    if matches!(event.status, "mismatch" | "passed" | "read_failed") {
                        terminal.push((event.status, event.actual_sha1.clone(), event.files_total));
                    }
                },
            );
            assert!(result.is_err());
            if fail {
                assert_eq!(tape.reads, 1);
                assert_eq!(terminal, vec![("read_failed", None, 2)]);
            } else {
                assert_eq!(tape.reads, 2);
                assert_eq!(
                    terminal.iter().map(|e| e.0).collect::<Vec<_>>(),
                    vec!["mismatch", "passed"]
                );
                assert!(terminal.iter().all(|e| e.1.as_deref()
                    == Some("a9993e364706816aba3e25717850c26c9cd0d89d")
                    && e.2 == 2));
            }
        }
    }
    #[test]
    fn cancellation_at_file_start_issues_no_read() {
        let mut f = file("a9993e364706816aba3e25717850c26c9cd0d89d");
        f.extents[0].byte_offset = 0;
        let token = CancellationToken::default();
        let mut tape = FakeTape {
            position: 0,
            reads: 0,
            fail: false,
        };
        let result = audit_files(
            &mut tape,
            vec![("/a".into(), &f, f.extended_attributes[0].value.clone())],
            &BTreeMap::new(),
            &token,
            &mut |event| {
                if event.status == "started" {
                    token.request_cancel();
                }
            },
        );
        assert!(result.unwrap_err().contains("[cancelled]"));
        assert_eq!(tape.reads, 0);
    }
    #[test]
    fn partial_record_recycling_does_not_geometrically_grow_allocations() {
        struct PartialTape;
        impl ExtentTape for PartialTape {
            fn locate_extent(&mut self, _: u8, _: u64) -> Result<(), String> {
                Ok(())
            }
            fn read_extent_record(&mut self, mut buffer: Vec<u8>) -> Result<Vec<u8>, String> {
                buffer.resize(crate::device::tape::READ_BUF_LEN, 7);
                assert_eq!(buffer.capacity(), crate::device::tape::READ_BUF_LEN);
                Ok(buffer)
            }
        }
        let payload = vec![7; 600_000];
        let expected = format!("{:x}", sha1::Sha1::digest(&payload));
        let mut pipeline =
            verify_pipeline::VerifyPipeline::spawn_sha1(CancellationToken::default()).unwrap();
        let mut cursor = ExtentReadCursor::default();
        for block in 0..40 {
            let mut f = file(&expected);
            f.length = 600_000;
            f.extents[0].start_block = block;
            f.extents[0].byte_offset = 0;
            f.extents[0].byte_count = 600_000;
            verify_file_hash_with_progress(
                &mut PartialTape,
                &f,
                ("/slice", &expected),
                &mut cursor,
                &mut pipeline,
                &mut |_| {},
            )
            .unwrap();
        }
    }

    fn index_with(files: Vec<FileEntry>) -> Index {
        Index {
            version: "2.4.0".into(),
            creator: "test".into(),
            volume_uuid: "test".into(),
            generation: 1,
            update_time: String::new(),
            self_location: TapePos {
                partition: 0,
                startblock: 5,
            },
            previous_location: None,
            allow_policy_update: false,
            volume_lock_state: None,
            highest_file_uid: None,
            root: crate::ltfs::index::Directory {
                entries: files.into_iter().map(DirectoryEntry::File).collect(),
                ..Default::default()
            },
        }
    }
    #[test]
    fn resume_checks_file_evidence_and_duplicate_paths_are_rejected() {
        let hash = "a9993e364706816aba3e25717850c26c9cd0d89d";
        let mut a = file(hash);
        a.extents[0].byte_offset = 0;
        let mut b = a.clone();
        b.name = "b".into();
        b.extents[0].start_block = 11;
        let index = index_with(vec![a.clone(), b]);
        let previous = BTreeMap::from([("/a".into(), (3, hash.to_string()))]);
        let files = plan_files(&index, &previous).unwrap();
        let mut tape = FakeTape {
            position: 0,
            reads: 0,
            fail: false,
        };
        let mut statuses = Vec::new();
        audit_files(
            &mut tape,
            files,
            &previous,
            &CancellationToken::default(),
            &mut |event| statuses.push(event.status),
        )
        .unwrap();
        assert_eq!(tape.reads, 1);
        assert_eq!(statuses, vec!["skipped_previous_pass", "started", "passed"]);
        assert!(
            plan_files(
                &index,
                &BTreeMap::from([("/a".into(), (4, hash.to_string()))])
            )
            .is_err()
        );
        assert!(
            plan_files(&index_with(vec![a.clone(), a]), &previous)
                .unwrap_err()
                .contains("重复文件路径")
        );
    }

    #[test]
    fn inventory_normalizes_sha1_and_preserves_shared_record_offset() {
        let dir = crate::ltfs::index::Directory {
            entries: vec![DirectoryEntry::File(file(
                "A9993E364706816ABA3E25717850C26C9CD0D89D",
            ))],
            ..Default::default()
        };
        let mut files = Vec::new();
        collect(&dir, "", &mut files).unwrap();
        assert_eq!(files[0].0, "/a");
        assert_eq!(files[0].1.extents[0].byte_offset, 7);
        assert_eq!(files[0].2, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
    #[test]
    fn missing_duplicate_malformed_hash_and_sparse_file_do_not_silently_pass() {
        let mut cases = Vec::new();
        let mut f = file("bad");
        cases.push(f.clone());
        f.extended_attributes.clear();
        cases.push(f);
        let mut f = file(&"a".repeat(40));
        f.extended_attributes.push(f.extended_attributes[0].clone());
        cases.push(f);
        let mut f = file(&"a".repeat(40));
        f.extents[0].file_offset = 1;
        cases.push(f);
        let mut f = file(&"a".repeat(40));
        f.length = 4;
        cases.push(f);
        let mut f = file(&"a".repeat(40));
        f.symlink_target = Some("target".into());
        cases.push(f);
        for f in cases {
            let dir = crate::ltfs::index::Directory {
                entries: vec![DirectoryEntry::File(f)],
                ..Default::default()
            };
            assert!(collect(&dir, "", &mut Vec::new()).is_err());
        }
    }
}
