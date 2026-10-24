//! Explicitly invoked hardware-host benchmarks; never run by normal cargo test.
use super::*;
use std::time::Instant;

#[test]
#[ignore = "explicit host filesystem benchmark; creates files only under supplied new destinations"]
fn host_small_files() {
    let spec: serde_json::Value = serde_json::from_str(
        &std::env::var("TAPECPY_HOST_BENCH_SPEC").expect("benchmark specification"),
    )
    .unwrap();
    let root = PathBuf::from(spec["root"].as_str().unwrap());
    let mode = spec["mode"].as_str().unwrap();
    let count = spec["count"].as_u64().unwrap() as usize;
    let size = spec["size"].as_u64().unwrap() as usize;
    let nested = spec["nested"].as_bool().unwrap();
    assert!(
        count > 0
            && size > 0
            && count
                .checked_mul(size)
                .is_some_and(|bytes| bytes <= 512 * 1024 * 1024)
    );
    let name = |i: usize| -> PathBuf {
        if nested {
            format!("group-{:03}/file-{i:06}.bin", i / 100).into()
        } else {
            format!("file-{i:06}.bin").into()
        }
    };
    let data = |i: usize| {
        let mut reader = PseudorandomReader::new(23456 + i as u64, size as u64);
        let mut value = vec![0; size];
        std::io::Read::read_exact(&mut reader, &mut value).unwrap();
        value
    };
    let started = Instant::now();
    let result = match mode {
        "generate" => {
            std::fs::create_dir(&root).unwrap();
            for i in 0..count {
                let path = root.join(name(i));
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let mut output = std::fs::File::create_new(path).unwrap();
                std::io::Write::write_all(&mut output, &data(i)).unwrap();
            }
            serde_json::json!({"generate_seconds":started.elapsed().as_secs_f64()})
        }
        "source" => {
            let scan = scan_source_roots(std::slice::from_ref(&root)).unwrap();
            assert_eq!(scan.files.len(), count);
            let scan_seconds = started.elapsed().as_secs_f64();
            let files = scan
                .files
                .iter()
                .map(|file| PlannedFile {
                    source: if spec["assume_stable"].as_bool().unwrap_or(false) {
                        PlannedSource::AssumedStable(file.path.clone())
                    } else {
                        PlannedSource::ValidatedLocal {
                            path: file.path.clone(),
                            identity: file.identity.clone(),
                        }
                    },
                    target: file.path.strip_prefix(&root).unwrap().display().to_string(),
                    size: file.size,
                })
                .collect();
            let stream = Instant::now();
            let pipeline = SourcePipeline::spawn(
                files,
                512 * 1024,
                DEFAULT_WRITE_BUFFER_BYTES,
                CancellationToken::default(),
            )
            .unwrap();
            let mut bytes = 0;
            let mut hashes = Vec::new();
            while hashes.len() < count {
                match pipeline.recv().unwrap() {
                    SourcePipelineMessage::Data(buffer) => {
                        bytes += buffer.len();
                        pipeline.recycle(buffer);
                    }
                    SourcePipelineMessage::FileEnd { sha256, .. } => hashes.push(sha256),
                    SourcePipelineMessage::Error(error) => panic!("{error}"),
                    SourcePipelineMessage::FileStart { .. } => (),
                }
            }
            pipeline.join().unwrap();
            let seconds = stream.elapsed().as_secs_f64();
            assert_eq!(bytes, count * size);
            // Independent reads outside timing verify the real pipeline's file hashes.
            for (file, expected) in scan.files.iter().zip(hashes) {
                let value = std::fs::read(&file.path).unwrap();
                assert_eq!(format!("{:x}", Sha256::digest(&value)), expected);
            }
            serde_json::json!({"scan_seconds":scan_seconds,"stream_seconds":seconds,
                "MB_s":bytes as f64/seconds/1e6,"files_s":count as f64/seconds,"verified":true})
        }
        "destination" => {
            // Same safe prepare/open/finish/sync methods used by the destination worker.
            let mut plan = ReadPlan {
                volume_uuid: "host-only".into(),
                index_generation: 0,
                selections: vec![],
                directories: vec![],
                files: vec![],
                extents: vec![],
                payload_bytes: (count * size) as u64,
            };
            for i in 0..count {
                plan.files.push(PlannedReadFile {
                    tape_path: name(i).display().to_string(),
                    relative_path: name(i),
                    length: size as u64,
                });
                plan.extents.push(PlannedReadExtent {
                    file_index: i,
                    file_offset: 0,
                    partition: 1,
                    start_block: i as u64,
                    byte_offset: 0,
                    byte_count: size as u64,
                });
            }
            // Pre-generate the bounded set of simulated input payloads, outside timing.
            let payloads: Vec<_> = (0..count).map(data).collect();
            std::fs::create_dir(&root).unwrap();
            let prepare = Instant::now();
            let mut outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
            let prepare_seconds = prepare.elapsed().as_secs_f64();
            let stream = Instant::now();
            for (i, payload) in payloads.iter().enumerate() {
                let mut output = outputs.open_extent(&plan, i).unwrap();
                std::io::Seek::seek(&mut output, std::io::SeekFrom::Start(0)).unwrap();
                std::io::Write::write_all(&mut output, payload).unwrap();
                assert!(outputs.finish_extent(&plan, i, output).unwrap());
            }
            let stream_seconds = stream.elapsed().as_secs_f64();
            let sync = Instant::now();
            outputs.sync_filesystem().unwrap();
            let sync_seconds = sync.elapsed().as_secs_f64();
            for (i, payload) in payloads.iter().enumerate() {
                assert_eq!(&std::fs::read(root.join(name(i))).unwrap(), payload);
            }
            serde_json::json!({"prepare_seconds":prepare_seconds,"stream_seconds":stream_seconds,
                "sync_seconds":sync_seconds,"MB_s":(count*size) as f64/stream_seconds/1e6,
                "files_s":count as f64/stream_seconds,"verified":true})
        }
        _ => panic!("unknown benchmark mode"),
    };
    println!(
        "BENCH_RESULT={}",
        serde_json::json!({"spec":spec,"result":result})
    );
}

thread_local! {
    static SCSI_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SCSI_TIMINGS: std::cell::RefCell<Vec<(u8, usize, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
    static WRITE_CLOCK: std::cell::RefCell<Option<Instant>> = const { std::cell::RefCell::new(None) };
    static WRITE_TIMINGS: std::cell::RefCell<Option<Vec<(usize, u64, u64)>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn record_scsi(opcode: u8, bytes: usize, elapsed: std::time::Duration) {
    super::restore_tests::record_device(opcode, elapsed);
    if matches!(opcode, 0x08 | 0x0a) && SCSI_ACTIVE.get() {
        SCSI_TIMINGS
            .with_borrow_mut(|values| values.push((opcode, bytes, elapsed.as_nanos() as u64)));
    }
}

pub(super) fn record_write(size: usize, elapsed: std::time::Duration) {
    WRITE_TIMINGS.with_borrow_mut(|timings| {
        if let Some(timings) = timings {
            let wall = WRITE_CLOCK.with_borrow(|clock| clock.unwrap().elapsed().as_nanos() as u64);
            timings.push((size, elapsed.as_nanos().min(u64::MAX as u128) as u64, wall));
        }
    });
}

pub(super) fn record_plan(index: &mut Index, target: &str, now: &str) -> Result<WritePlan, String> {
    if std::env::var_os("TAPECPY_RESTORE_FIXTURE").is_some() {
        return super::restore_tests::fixture_plan(index, target, now);
    }
    let (parent, name) = split_target_path(target)?;
    let uid = index.highest_file_uid.unwrap_or(0) + 1;
    let directory = index.find_directory_mut(&parent).ok_or("missing parent")?;
    if directory.entries.iter().any(|entry| entry.name() == name) {
        return Err("benchmark target exists".into());
    }
    directory
        .entries
        .push(DirectoryEntry::Directory(crate::ltfs::index::Directory {
            name,
            fileuid: uid,
            times: planned_times(now),
            ..Default::default()
        }));
    index.highest_file_uid = Some(uid);
    let mut result = WritePlan {
        highest_uid: uid,
        directories: 1,
        ..Default::default()
    };
    for repeat in 0..3 {
        let sizes = if repeat % 2 == 0 {
            [4096, 65536, 524288]
        } else {
            [524288, 65536, 4096]
        };
        for size in sizes {
            let group = format!("r{repeat}-{size}");
            result.highest_uid += 1;
            let group_uid = result.highest_uid;
            index
                .find_directory_mut(target.trim_start_matches('/'))
                .unwrap()
                .entries
                .push(DirectoryEntry::Directory(crate::ltfs::index::Directory {
                    name: group.clone(),
                    fileuid: group_uid,
                    times: planned_times(now),
                    ..Default::default()
                }));
            index.highest_file_uid = Some(group_uid);
            result.directories += 1;
            for i in 0..64 * 1024 * 1024 / size {
                let path = format!("{target}/{group}/file-{i:06}.bin");
                let part =
                    plan_pseudorandom_file(index, &path, size as u64, 34567 + i as u64, now)?;
                index.highest_file_uid = Some(part.highest_uid);
                result.highest_uid = part.highest_uid;
                result.total_bytes += part.total_bytes;
                result.files.extend(part.files);
            }
        }
    }
    Ok(result)
}

#[test]
#[ignore = "explicit tape benchmark; appends a new LTFS directory and preserves existing data"]
fn tape_record_sizes() {
    let target = std::env::var("TAPECPY_RECORD_TARGET").expect("new target required");
    let drives = discover_drives().unwrap();
    let drive = select_drive(&drives, Some("/dev/sg0")).unwrap();
    let _lease = device::lease::DeviceLease::try_acquire(
        &drive.serial,
        device::lease::LeaseOwner::new("hardware-test", "record-size-benchmark"),
    )
    .unwrap();
    WRITE_CLOCK.with_borrow_mut(|clock| *clock = Some(Instant::now()));
    WRITE_TIMINGS.with_borrow_mut(|timings| *timings = Some(Vec::new()));
    let wall = Instant::now();
    let mut observer = |event: &WriteEvent| {
        SCSI_ACTIVE.set(event.phase == WritePhase::WritingData);
        if let Some(sample) = &event.telemetry {
            println!(
                "RECORD_TELEMETRY={}",
                serde_json::json!({"bytes":event.bytes_written,"temperature":sample.temperature,"worst_rate":sample.worst_rate})
            );
        }
    };
    let result = write_with_observer_detailed(
        drive,
        WriteSourceRequest::RecordBenchmark,
        &target,
        WriteOptions::default(),
        &mut observer,
    )
    .unwrap();
    SCSI_ACTIVE.set(false);
    let timings = WRITE_TIMINGS.with_borrow_mut(Option::take).unwrap();
    println!(
        "RECORD_WRITE={}",
        serde_json::json!({"wall_seconds":wall.elapsed().as_secs_f64(),
        "bytes":result.bytes,"generation":result.generation,"timings_size_ns":timings,
        "warnings":result.telemetry_warnings})
    );
    // Read only the new contiguous records. Positioning is outside command timing.
    let volume = inspect_volume(drive).unwrap();
    let index = volume.index.unwrap();
    fn collect_hashes(
        directory: &crate::ltfs::index::Directory,
        prefix: &str,
        hashes: &mut std::collections::HashMap<String, String>,
    ) {
        for entry in &directory.entries {
            match entry {
                DirectoryEntry::File(file) => {
                    if let Some(hash) = file
                        .extended_attributes
                        .iter()
                        .find(|a| a.key == "ltfs.hash.sha256sum")
                    {
                        hashes.insert(format!("{prefix}/{}", file.name), hash.value.clone());
                    }
                }
                DirectoryEntry::Directory(child) => {
                    collect_hashes(child, &format!("{prefix}/{}", child.name), hashes)
                }
            }
        }
    }
    let mut hashes = std::collections::HashMap::new();
    collect_hashes(&index.root, "", &mut hashes);
    let plan = plan_ltfs_read(&index, std::slice::from_ref(&target)).unwrap();
    let mut tape = TapeSession::open(&drive.sg_path).unwrap();
    let mut read_timings = Vec::new();
    let mut position = None;
    let mut buffer = Vec::with_capacity(device::tape::READ_BUF_LEN);
    for extent in &plan.extents {
        assert_eq!(extent.byte_offset, 0);
        if position != Some((extent.partition, extent.start_block)) {
            tape.locate(extent.partition, extent.start_block).unwrap();
        }
        SCSI_ACTIVE.set(true);
        let started = Instant::now();
        let ReadRecord::Data(data) = tape.read_record_into(buffer).unwrap() else {
            panic!("expected data");
        };
        SCSI_ACTIVE.set(false);
        read_timings.push((data.len(), started.elapsed().as_nanos() as u64));
        assert_eq!(data.len() as u64, extent.byte_count);
        let expected = hashes
            .get(&plan.files[extent.file_index].tape_path)
            .unwrap();
        assert_eq!(&format!("{:x}", Sha256::digest(&data)), expected);
        position = Some((extent.partition, extent.start_block + 1));
        buffer = data;
    }
    let scsi_timings = SCSI_TIMINGS.with_borrow_mut(std::mem::take);
    println!(
        "RECORD_SCSI={}",
        serde_json::json!({"opcode_size_ns":scsi_timings})
    );
    println!(
        "RECORD_READ={}",
        serde_json::json!({"timings_size_ns":read_timings,"verified_files":plan.files.len(),"bytes":plan.payload_bytes})
    );
}

#[test]
fn record_fixture_plan_preserves_existing_files_and_assigns_unique_uids() {
    let mut index = super::tests::empty_index();
    index.root.entries.push(DirectoryEntry::File(FileEntry {
        name: "existing.bin".into(),
        fileuid: 2,
        length: 7,
        extents: vec![Extent {
            partition: 1,
            start_block: 123,
            byte_count: 7,
            ..Default::default()
        }],
        ..Default::default()
    }));
    index.highest_file_uid = Some(2);
    let old_root = index.root.clone();
    let plan = record_plan(&mut index, "/record-fixture", "2026-10-24T00:00:00Z").unwrap();
    assert_eq!(plan.files.len(), 52608);
    assert_eq!(plan.total_bytes, 576 * 1024 * 1024);
    assert_eq!(
        &index.root.entries[..old_root.entries.len()],
        old_root.entries.as_slice()
    );
    let mut targets = std::collections::HashSet::new();
    let mut uids = std::collections::HashSet::new();
    for file in &plan.files {
        assert!(targets.insert(file.target.clone()));
        let entry = index.find_file(&file.target).unwrap();
        assert!(uids.insert(entry.fileuid));
    }
    assert!(record_plan(&mut index, "/record-fixture", "test").is_err());
}

#[test]
#[ignore = "explicit source prefill/RSS comparison, no tape"]
fn source_prefill_comparison() {
    let files = (0..10000)
        .map(|i| PlannedFile {
            source: PlannedSource::Pseudorandom { seed: i },
            target: format!("/{i}"),
            size: 4096,
        })
        .collect();
    let pipeline = SourcePipeline::spawn(
        files,
        524288,
        DEFAULT_WRITE_BUFFER_BYTES,
        CancellationToken::default(),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let snapshot = pipeline.snapshot();
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let rss = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .unwrap()
        .to_string();
    let mut completed = 0;
    let mut payload = Vec::new();
    let mut bytes = 0;
    while completed < 10000 {
        match pipeline.recv().unwrap() {
            SourcePipelineMessage::FileStart { .. } => payload.clear(),
            SourcePipelineMessage::Data(buffer) => {
                bytes += buffer.len();
                payload.extend_from_slice(&buffer);
                pipeline.recycle(buffer);
            }
            SourcePipelineMessage::FileEnd { sha256, .. } => {
                assert_eq!(sha256, format!("{:x}", Sha256::digest(&payload)));
                completed += 1;
            }
            SourcePipelineMessage::Error(error) => panic!("{error}"),
        }
    }
    pipeline.join().unwrap();
    println!(
        "PREFILL_RESULT={}",
        serde_json::json!({"queued_payload_bytes":snapshot.queued_bytes,
        "source_bytes":snapshot.bytes_read,"capacity_bytes":snapshot.capacity_bytes,
        "peak_rss":rss,"verified_files":completed,"verified_bytes":bytes})
    );
}

#[test]
#[ignore = "explicit normal LTFS write+readback comparison, appends a NEW target"]
fn source_write_comparison() {
    let spec: serde_json::Value =
        serde_json::from_str(&std::env::var("TAPECPY_WRITE_COMPARE_SPEC").unwrap()).unwrap();
    let source = PathBuf::from(spec["source"].as_str().unwrap());
    let target = spec["target"].as_str().unwrap();
    let drives = discover_drives().unwrap();
    let drive = select_drive(&drives, Some("/dev/sg0")).unwrap();
    let _lease = device::lease::DeviceLease::try_acquire(
        &drive.serial,
        device::lease::LeaseOwner::new("hardware-test", "source-optimization-comparison"),
    )
    .unwrap();
    let clock = Instant::now();
    let mut previous = None;
    let mut phases = Vec::new();
    let mut samples = Vec::new();
    let mut telemetry = Vec::new();
    let mut observer = |event: &WriteEvent| {
        if previous != Some(event.phase) {
            phases.push(serde_json::json!({"phase":format!("{:?}",event.phase),"seconds":clock.elapsed().as_secs_f64(),"bytes":event.bytes_written}));
            previous = Some(event.phase);
        }
        if let Some(performance) = &event.performance {
            samples.push(serde_json::json!({"seconds":clock.elapsed().as_secs_f64(),"bytes":event.bytes_written,"tape_bps":performance.tape_bytes_per_second,"source_bps":performance.source_bytes_per_second,"buffer":performance.buffer_used_bytes,"capacity":performance.buffer_capacity_bytes}));
        }
        if let Some(sample) = &event.telemetry {
            telemetry.push(serde_json::json!({"timestamp":sample.timestamp,"temperature":sample.temperature,"channel_rates":sample.channel_rates,"counter_window":sample.counter_window,"worst_rate":sample.worst_rate}));
        }
    };
    let result = WriteSession::new(drive)
        .run_detailed_with_options(
            &source,
            target,
            WriteOptions {
                verification: WriteVerification::ReadBackSha256,
                ..Default::default()
            },
            &mut observer,
        )
        .unwrap();
    assert_eq!(result.verification, WriteVerification::ReadBackSha256);
    println!(
        "WRITE_COMPARE_RESULT={}",
        serde_json::json!({"spec":spec,"phases":phases,"performance":samples,"telemetry":telemetry,
        "wall_seconds":clock.elapsed().as_secs_f64(),"files":result.files,"bytes":result.bytes,
        "generation":result.generation,"hashes":result.hashes.len(),"verification":"passed",
        "health_delta":format!("{:?}",result.health_delta),"warnings":result.telemetry_warnings})
    );
}
