use super::*;
use std::time::{Duration, Instant};

fn directory(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "tapecpy-source-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    root
}

#[test]
fn validated_source_rejects_replacement_symlink_and_in_place_changes() {
    let root = directory("identity");
    let path = root.join("file");
    std::fs::write(&path, b"abcd").unwrap();
    let (_, identity) = open_regular_source(&path).unwrap();
    let source = PlannedSource::ValidatedLocal {
        path: path.clone(),
        identity,
    };
    let opened = source.open(4).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(
        std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(100)),
    )
    .unwrap();
    assert!(opened.finish().unwrap_err().contains("变化"));
    assert!(source.open(4).is_err());
    std::fs::rename(&path, root.join("old")).unwrap();
    std::fs::write(&path, b"abcd").unwrap();
    assert!(source.open(4).is_err());
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(root.join("old"), &path).unwrap();
    assert!(source.open(4).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn raw_source_keeps_symlink_support_and_checks_size() {
    let root = directory("raw");
    let path = root.join("file");
    std::fs::write(&path, b"abcd").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(PlannedSource::Local(link.clone()).open(4).is_ok());
    assert!(PlannedSource::Local(link).open(5).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn small_sources_prefill_payload_instead_of_full_tape_block_allocations() {
    let count = 100;
    let size = 4096;
    let files = (0..count)
        .map(|i| PlannedFile {
            source: PlannedSource::Pseudorandom { seed: i },
            target: format!("/{i}"),
            size,
        })
        .collect();
    let pipeline =
        SourcePipeline::spawn(files, 512 * 1024, 512 * 1024, CancellationToken::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while pipeline.snapshot().bytes_read < count * size && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(pipeline.snapshot().queued_bytes, count * size);
    assert!(
        pipeline
            .metrics
            .peak_allocated_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
            <= 512 * 1024
    );
    let mut completed = 0;
    while completed < count {
        match pipeline.recv().unwrap() {
            SourcePipelineMessage::Data(data) => pipeline.recycle(data),
            SourcePipelineMessage::FileEnd { .. } => completed += 1,
            SourcePipelineMessage::FileStart { .. } => (),
            SourcePipelineMessage::Error(error) => panic!("{error}"),
        }
    }
    pipeline.join().unwrap();
}

#[test]
fn mixed_source_allocations_stay_within_budget_and_keep_record_boundaries() {
    let sizes = [0, 1, 4096, 4097, 32768, 7, 65536, 16383, 1];
    let files = sizes
        .iter()
        .enumerate()
        .map(|(i, &size)| PlannedFile {
            source: PlannedSource::Pseudorandom { seed: i as u64 },
            target: format!("/{i}"),
            size,
        })
        .collect();
    let pipeline =
        SourcePipeline::spawn(files, 16384, 32768, CancellationToken::default()).unwrap();
    let mut i = 0;
    let mut payload = Vec::new();
    while i < sizes.len() {
        match pipeline.recv().unwrap() {
            SourcePipelineMessage::FileStart { .. } => payload.clear(),
            SourcePipelineMessage::Data(data) => {
                assert_eq!(
                    data.len(),
                    (sizes[i] - payload.len() as u64).min(16384) as usize
                );
                payload.extend_from_slice(&data);
                pipeline.recycle(data);
            }
            SourcePipelineMessage::FileEnd {
                actual_size,
                sha256,
            } => {
                assert_eq!(actual_size, sizes[i]);
                assert_eq!(sha256, format!("{:x}", Sha256::digest(&payload)));
                i += 1;
            }
            SourcePipelineMessage::Error(error) => panic!("{error}"),
        }
    }
    assert!(
        pipeline
            .metrics
            .peak_allocated_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
            <= 32768
    );
    assert_eq!(pipeline.snapshot().queued_bytes, 0);
    pipeline.join().unwrap();
}

#[test]
fn source_cancellation_interrupts_budget_wait() {
    let cancellation = CancellationToken::default();
    let pipeline = SourcePipeline::spawn(
        vec![PlannedFile {
            source: PlannedSource::Pseudorandom { seed: 1 },
            target: "/one".into(),
            size: 1024,
        }],
        4,
        8,
        cancellation.clone(),
    )
    .unwrap();
    assert!(matches!(
        pipeline.recv().unwrap(),
        SourcePipelineMessage::FileStart { .. }
    ));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !pipeline.snapshot().reader_waiting && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(pipeline.snapshot().reader_waiting);
    cancellation.request_cancel();
    loop {
        match pipeline
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .expect("cancellation event")
        {
            SourcePipelineMessage::Error(error) => {
                assert!(error.contains("[cancelled]"));
                break;
            }
            SourcePipelineMessage::Data(_) => (),
            _ => (),
        }
    }
    pipeline.join().unwrap();
}

#[test]
fn source_mutation_while_backpressured_never_publishes_file_end() {
    for (name, initial, changed) in [("shrink", 64, 2), ("grow", 8, 9)] {
        let root = directory(name);
        let path = root.join("file");
        std::fs::write(&path, vec![7; initial]).unwrap();
        let (_, identity) = open_regular_source(&path).unwrap();
        let pipeline = SourcePipeline::spawn(
            vec![PlannedFile {
                source: PlannedSource::ValidatedLocal {
                    path: path.clone(),
                    identity,
                },
                target: "/file".into(),
                size: initial as u64,
            }],
            4,
            4,
            CancellationToken::default(),
        )
        .unwrap();
        assert!(matches!(
            pipeline.recv().unwrap(),
            SourcePipelineMessage::FileStart { .. }
        ));
        let first = match pipeline.recv().unwrap() {
            SourcePipelineMessage::Data(buffer) => buffer,
            _ => panic!("expected first record"),
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !pipeline.snapshot().reader_waiting && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(pipeline.snapshot().reader_waiting);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(changed)
            .unwrap();
        pipeline.recycle(first);
        loop {
            match pipeline
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .expect("mutation error")
            {
                SourcePipelineMessage::Data(buffer) => pipeline.recycle(buffer),
                SourcePipelineMessage::Error(error) => {
                    assert!(error.contains("变化"));
                    break;
                }
                SourcePipelineMessage::FileEnd { .. } => {
                    panic!("mutated source was published as complete")
                }
                SourcePipelineMessage::FileStart { .. } => panic!("unexpected source"),
            }
        }
        pipeline.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn assumed_stable_experiment_still_rejects_short_reads() {
    let root = directory("stable-short");
    let path = root.join("file");
    std::fs::write(&path, b"abc").unwrap();
    let pipeline = SourcePipeline::spawn(
        vec![PlannedFile {
            source: PlannedSource::AssumedStable(path),
            target: "/file".into(),
            size: 4,
        }],
        512 * 1024,
        DEFAULT_WRITE_BUFFER_BYTES,
        CancellationToken::default(),
    )
    .unwrap();
    loop {
        match pipeline.recv().unwrap() {
            SourcePipelineMessage::Error(error) => {
                assert!(error.contains("short read"));
                break;
            }
            SourcePipelineMessage::FileEnd { .. } => panic!("truncated source accepted"),
            SourcePipelineMessage::Data(buffer) => pipeline.recycle(buffer),
            SourcePipelineMessage::FileStart { .. } => (),
        }
    }
    pipeline.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
