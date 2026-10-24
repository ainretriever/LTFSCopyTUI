//! Explicit restore research. Hooks are absent from production builds.
use super::*;
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
static ACTIVE: AtomicBool = AtomicBool::new(false);
thread_local! { static IN_DATA_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[derive(Default)]
struct Measurements {
    operations: std::collections::BTreeMap<&'static str, Vec<u64>>,
    read_end: Option<Instant>,
    clock: Option<Instant>,
    first_read: Option<f64>,
    last_read: Option<f64>,
    long_device_reads: Vec<serde_json::Value>,
}
static MEASUREMENTS: OnceLock<Mutex<Measurements>> = OnceLock::new();
fn state() -> &'static Mutex<Measurements> {
    MEASUREMENTS.get_or_init(Default::default)
}
pub(super) struct Probe {
    name: &'static str,
    start: Option<Instant>,
}
impl Probe {
    pub(super) fn new(name: &'static str) -> Self {
        if name == "tape_read" {
            IN_DATA_READ.set(ACTIVE.load(Ordering::Relaxed));
        }
        Self {
            name,
            start: ACTIVE.load(Ordering::Relaxed).then(Instant::now),
        }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        if self.name == "tape_read" {
            IN_DATA_READ.set(false);
        }
        if let Some(start) = self.start {
            let now = Instant::now();
            let mut s = state().lock().unwrap();
            s.operations
                .entry(self.name)
                .or_default()
                .push(now.duration_since(start).as_nanos() as u64);
            if self.name == "tape_read" {
                s.read_end = Some(now);
                s.last_read = s.clock.map(|c| now.duration_since(c).as_secs_f64());
            }
        }
    }
}
pub(super) fn record_device(opcode: u8, elapsed: Duration) {
    if opcode == 0x08 && IN_DATA_READ.get() && ACTIVE.load(Ordering::Relaxed) {
        let mut s = state().lock().unwrap();
        let values = s.operations.entry("sg_io_read").or_default();
        values.push(elapsed.as_nanos() as u64);
        let sequence = values.len();
        if elapsed >= Duration::from_millis(100) {
            let seconds = s.clock.map(|c| c.elapsed().as_secs_f64());
            s.long_device_reads.push(serde_json::json!({"sequence":sequence,"end_seconds":seconds,"milliseconds":elapsed.as_secs_f64()*1000.0}));
        }
    }
}
pub(super) fn read_start() {
    if ACTIVE.load(Ordering::Relaxed) {
        let mut s = state().lock().unwrap();
        if s.first_read.is_none() {
            s.first_read = s.clock.map(|c| c.elapsed().as_secs_f64());
        }
        if let Some(end) = s.read_end.take() {
            s.operations
                .entry("between_reads")
                .or_default()
                .push(end.elapsed().as_nanos() as u64);
        }
    }
}
fn begin(enabled: bool) {
    *state().lock().unwrap() = Measurements {
        clock: Some(Instant::now()),
        ..Default::default()
    };
    ACTIVE.store(enabled, Ordering::Relaxed);
}
fn end() -> serde_json::Value {
    ACTIVE.store(false, Ordering::Relaxed);
    let mut s = state().lock().unwrap();
    let mut result = serde_json::Map::new();
    if !s.long_device_reads.is_empty() {
        result.insert(
            "long_device_reads".into(),
            serde_json::json!(s.long_device_reads),
        );
    }
    if let (Some(first), Some(last)) = (s.first_read, s.last_read) {
        result.insert("read_span".into(),serde_json::json!({"first_start_seconds":first,"last_end_seconds":last,"span_seconds":last-first}));
    }
    for (name, values) in &mut s.operations {
        values.sort_unstable();
        let n = values.len();
        result.insert(name.to_string(),serde_json::json!({"count":n,"total_seconds":values.iter().map(|x|*x as f64/1e9).sum::<f64>(),"p50_us":values[n/2] as f64/1e3,"p95_us":values[(n-1)*95/100] as f64/1e3,"p99_us":values[(n-1)*99/100] as f64/1e3,"max_ms":values[n-1] as f64/1e6}));
    }
    result.into()
}
fn spec() -> serde_json::Value {
    serde_json::from_str(&std::env::var("TAPECPY_RESTORE_SPEC").unwrap()).unwrap()
}
pub(super) fn host_plan(size: u64, count: usize, record: u64, nested: bool) -> ReadPlan {
    let mut p = ReadPlan {
        volume_uuid: "restore-host".into(),
        index_generation: 0,
        selections: vec![],
        directories: vec![],
        files: vec![],
        extents: vec![],
        payload_bytes: size * count as u64,
    };
    for i in 0..count {
        let name = if nested {
            format!("group-{:03}/file-{i:06}.bin", i / 100)
        } else {
            format!("file-{i:06}.bin")
        };
        p.files.push(PlannedReadFile {
            tape_path: format!("/{name}"),
            relative_path: name.into(),
            length: size,
        });
        // One contiguous extent per file, many records inside it.
        p.extents.push(PlannedReadExtent {
            file_index: i,
            file_offset: 0,
            partition: 1,
            start_block: i as u64 * size.div_ceil(record),
            byte_offset: 0,
            byte_count: size,
        });
    }
    p
}
fn verify_pool(plan: &ReadPlan, root: &Path, pool: &[u8], record: usize) {
    for (i, f) in plan.files.iter().enumerate() {
        let mut input = std::fs::File::open(root.join(&f.relative_path)).unwrap();
        let mut expected = Sha256::new();
        let mut actual = Sha256::new();
        let mut remaining = f.length as usize;
        let mut offset = i * f.length as usize;
        let mut buf = vec![0; record];
        while remaining > 0 {
            let n = remaining.min(record).min(pool.len() - offset % pool.len());
            std::io::Read::read_exact(&mut input, &mut buf[..n]).unwrap();
            actual.update(&buf[..n]);
            expected.update(&pool[offset % pool.len()..offset % pool.len() + n]);
            remaining -= n;
            offset += n;
        }
        assert_eq!(actual.finalize(), expected.finalize());
        assert_eq!(input.metadata().unwrap().len(), f.length);
    }
}
#[test]
#[ignore = "explicit real restore output pipeline benchmark; new destination only"]
fn restore_host() {
    let spec = spec();
    let size = spec["size"].as_u64().unwrap();
    let count = spec["count"].as_u64().unwrap() as usize;
    let record = spec["record"].as_u64().unwrap_or(524288) as usize;
    assert!(record > 0 && record <= device::tape::READ_BUF_LEN);
    let root = PathBuf::from(spec["root"].as_str().unwrap());
    std::fs::create_dir(&root).unwrap();
    let plan = host_plan(
        size,
        count,
        record as u64,
        spec["nested"].as_bool().unwrap_or(false),
    );
    let mut pool = vec![0; 64 * 1024 * 1024];
    std::io::Read::read_exact(
        &mut PseudorandomReader::new(93845, pool.len() as u64),
        &mut pool,
    )
    .unwrap();
    begin(spec["instrument"].as_bool().unwrap_or(true));
    let clock = Instant::now();
    validate_read_plan_destination(&plan, &root).unwrap();
    let validated = clock.elapsed().as_secs_f64();
    let outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    let prepared = clock.elapsed().as_secs_f64();
    let pipeline = ReadDestinationPipeline::spawn(
        plan.clone(),
        outputs,
        CancellationToken::default(),
        DEFAULT_READ_BUFFER_BYTES,
    )
    .unwrap();
    let mut allocated = 0;
    let mut samples = Vec::new();
    let mut sampled = Instant::now();
    for i in 0..count {
        pipeline
            .send(ReadPipelineMessage::ExtentStart {
                file_index: i,
                file_offset: 0,
            })
            .unwrap();
        let mut left = size as usize;
        let mut offset = i * size as usize;
        while left > 0 {
            let mut buffer = pipeline.acquire_buffer(&mut allocated).unwrap();
            let n = left.min(record).min(pool.len() - offset % pool.len());
            buffer.clear();
            buffer.extend_from_slice(&pool[offset % pool.len()..offset % pool.len() + n]);
            pipeline.send_data(buffer).unwrap();
            left -= n;
            offset += n;
            if sampled.elapsed() >= Duration::from_millis(100) {
                let b = pipeline.snapshot();
                samples.push(serde_json::json!({"seconds":clock.elapsed().as_secs_f64(),"queued":b.used_bytes,"written":b.written_bytes,"waiting":b.tape_waiting}));
                sampled = Instant::now();
            }
        }
        pipeline.send(ReadPipelineMessage::ExtentEnd).unwrap();
    }
    let submitted = clock.elapsed().as_secs_f64();
    let slabs = pipeline.producer.borrow().allocated;
    pipeline.finish().unwrap();
    let finished = clock.elapsed().as_secs_f64();
    let operations = end();
    verify_pool(&plan, &root, &pool, record);
    println!(
        "RESTORE_RESULT={}",
        serde_json::json!({"kind":"host","spec":spec,"validate_seconds":validated,"prepare_seconds":prepared-validated,"submit_seconds":submitted-prepared,"drain_sync_seconds":finished-submitted,"total_seconds":finished,"bytes":plan.payload_bytes,"allocated_buffers":allocated,"allocated_payload_slabs":slabs,"operations":operations,"samples":samples,"verified":true})
    );
}
#[test]
#[ignore = "explicit bounded restore queue probe without filesystem or tape"]
fn restore_queue() {
    let spec = spec();
    let size = spec["size"].as_u64().unwrap();
    let record = spec["record"].as_u64().unwrap() as usize;
    let root = std::env::temp_dir().join(format!("tapecpy-restore-queue-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let count = DEFAULT_READ_BUFFER_BYTES as u64 / size + 16;
    let mut plan = host_plan(size, 1, record as u64, false);
    let template = plan.extents[0].clone();
    plan.extents = (0..count)
        .map(|i| PlannedReadExtent {
            file_offset: i * size,
            start_block: i * size.div_ceil(record as u64),
            ..template.clone()
        })
        .collect();
    plan.files[0].length = size * count;
    plan.payload_bytes = size * count;
    let outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    let cancellation = CancellationToken::default();
    PAUSE_MS.store(3000, Ordering::Relaxed);
    let pipeline = ReadDestinationPipeline::spawn(
        plan.clone(),
        outputs,
        cancellation.clone(),
        DEFAULT_READ_BUFFER_BYTES,
    )
    .unwrap();
    let m = pipeline.metrics.clone();
    let sampler = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !m.tape_waiting.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let snapshot = serde_json::json!({"queued_bytes":m.queued_bytes.load(Ordering::Relaxed),"queued_records":m.queued_records.load(Ordering::Relaxed),"waiting":m.tape_waiting.load(Ordering::Relaxed)});
        cancellation.request_cancel();
        snapshot
    });
    let mut allocated = 0;
    let production = (|| -> Result<(), String> {
        for extent in &plan.extents {
            pipeline.send(ReadPipelineMessage::ExtentStart {
                file_index: extent.file_index,
                file_offset: 0,
            })?;
            let mut left = size as usize;
            while left > 0 {
                let mut b = pipeline.acquire_buffer(&mut allocated)?;
                let n = left.min(record);
                b.resize(n, 0);
                pipeline.send_data(b)?;
                left -= n;
            }
            pipeline.send(ReadPipelineMessage::ExtentEnd)?;
        }
        Ok(())
    })();
    assert!(production.is_err());
    let slabs = pipeline.producer.borrow().allocated;
    assert!(pipeline.abort().unwrap_err().contains("cancelled"));
    let snapshot = sampler.join().unwrap();
    assert!(snapshot["waiting"].as_bool().unwrap());
    std::fs::remove_dir_all(root).unwrap();
    println!(
        "RESTORE_RESULT={}",
        serde_json::json!({"kind":"queue-actual","spec":spec,"snapshot":snapshot,"allocated_buffers":allocated,"allocated_payload_slabs":slabs,"configured_bytes":DEFAULT_READ_BUFFER_BYTES,"verified":true})
    );
}
static PAUSE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(super) fn pause_destination() {
    let ms = PAUSE_MS.swap(0, Ordering::Relaxed);
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms));
    }
}
pub(super) fn fixture_plan(
    index: &mut Index,
    target: &str,
    now: &str,
) -> Result<WritePlan, String> {
    let (parent, name) = split_target_path(target)?;
    let uid = index.highest_file_uid.unwrap_or(0) + 1;
    let d = index.find_directory_mut(&parent).ok_or("missing parent")?;
    if d.entries.iter().any(|e| e.name() == name) {
        return Err("fixture exists".into());
    }
    d.entries
        .push(DirectoryEntry::Directory(crate::ltfs::index::Directory {
            name,
            fileuid: uid,
            times: planned_times(now),
            ..Default::default()
        }));
    index.highest_file_uid = Some(uid);
    let mut plan = WritePlan {
        highest_uid: uid,
        directories: 1,
        ..Default::default()
    };
    for size in [1048576u64, 4194304, 16777216, 67108864] {
        let group = format!("{target}/size-{size}");
        let uid = plan.highest_uid + 1;
        index
            .find_directory_mut(target)
            .unwrap()
            .entries
            .push(DirectoryEntry::Directory(crate::ltfs::index::Directory {
                name: format!("size-{size}"),
                fileuid: uid,
                times: planned_times(now),
                ..Default::default()
            }));
        index.highest_file_uid = Some(uid);
        plan.highest_uid = uid;
        plan.directories += 1;
        for i in 0..8 * 1024 * 1024 * 1024 / size {
            let part = plan_pseudorandom_file(
                index,
                &format!("{group}/file-{i:06}.bin"),
                size,
                78912 + i,
                now,
            )?;
            index.highest_file_uid = Some(part.highest_uid);
            plan.highest_uid = part.highest_uid;
            plan.total_bytes += part.total_bytes;
            plan.files.extend(part.files);
        }
    }
    Ok(plan)
}
fn drive() -> TapeDrive {
    let drives = discover_drives().unwrap();
    select_drive(&drives, Some("/dev/sg0")).unwrap().clone()
}
#[test]
#[ignore = "explicit append 32GiB restore fixture, preserves existing tape data"]
fn restore_fixture() {
    let spec = spec();
    let drive = drive();
    let _lease = device::lease::DeviceLease::try_acquire(
        &drive.serial,
        device::lease::LeaseOwner::new("hardware-test", "restore-fixture"),
    )
    .unwrap();
    let mut samples = Vec::new();
    let mut last = Instant::now();
    let clock = Instant::now();
    let result=write_with_observer_detailed(&drive,WriteSourceRequest::RecordBenchmark,spec["target"].as_str().unwrap(),WriteOptions::default(),&mut |e| {
        if last.elapsed()>Duration::from_secs(5) {samples.push(serde_json::json!({"seconds":clock.elapsed().as_secs_f64(),"phase":format!("{:?}",e.phase),"bytes":e.bytes_written}));last=Instant::now();}
    }).unwrap();
    println!(
        "RESTORE_RESULT={}",
        serde_json::json!({"kind":"fixture","bytes":result.bytes,"files":result.files,"generation":result.generation,"seconds":clock.elapsed().as_secs_f64(),"samples":samples,"health_delta":format!("{:?}",result.health_delta)})
    );
}
fn verify_tape_hashes(index: &Index, plan: &ReadPlan, root: &Path) {
    for f in &plan.files {
        let expected = &index
            .find_file(&f.tape_path)
            .unwrap()
            .extended_attributes
            .iter()
            .find(|a| a.key == "ltfs.hash.sha256sum")
            .unwrap()
            .value;
        let mut file = std::fs::File::open(root.join(&f.relative_path)).unwrap();
        let mut sha = Sha256::new();
        let mut b = vec![0; 1048576];
        loop {
            let n = std::io::Read::read(&mut file, &mut b).unwrap();
            if n == 0 {
                break;
            }
            sha.update(&b[..n]);
        }
        assert_eq!(&format!("{:x}", sha.finalize()), expected);
        assert_eq!(file.metadata().unwrap().len(), f.length);
    }
}
#[test]
#[ignore = "explicit read-only tape restore to a new destination, all file hashes verified"]
fn restore_tape() {
    let spec = spec();
    let drive = drive();
    let _lease = device::lease::DeviceLease::try_acquire(
        &drive.serial,
        device::lease::LeaseOwner::new("hardware-test", "restore-research"),
    )
    .unwrap();
    let clock = Instant::now();
    let index = inspect_volume(&drive).unwrap().index.unwrap();
    let plan = plan_ltfs_read(&index, &[spec["target"].as_str().unwrap().to_string()]).unwrap();
    let planned = clock.elapsed().as_secs_f64();
    if spec["null"].as_bool().unwrap_or(false) {
        begin(true);
        let clock = Instant::now();
        let mut session = TapeSession::open(&drive.sg_path).unwrap();
        let mut cursor = ExtentReadCursor::default();
        let mut buffer = Vec::with_capacity(device::tape::READ_BUF_LEN);
        let mut bytes = 0u64;
        for extent in &plan.extents {
            assert_eq!(extent.file_offset, 0);
            let mut left = extent.byte_count;
            let mut block = extent.start_block;
            let mut offset = extent.byte_offset;
            let mut sha = Sha256::new();
            while left > 0 {
                buffer = cursor
                    .chunk(&mut session, extent.partition, block, offset, left, buffer)
                    .unwrap();
                sha.update(&buffer);
                left -= buffer.len() as u64;
                bytes += buffer.len() as u64;
                block += 1;
                offset = 0;
            }
            let expected = &index
                .find_file(&plan.files[extent.file_index].tape_path)
                .unwrap()
                .extended_attributes
                .iter()
                .find(|a| a.key == "ltfs.hash.sha256sum")
                .unwrap()
                .value;
            assert_eq!(&format!("{:x}", sha.finalize()), expected);
        }
        let seconds = clock.elapsed().as_secs_f64();
        let operations = end();
        println!(
            "RESTORE_RESULT={}",
            serde_json::json!({"kind":"tape-null","spec":spec,"bytes":bytes,"files":plan.files.len(),"planning_seconds":planned,"restore_seconds":seconds,"operations":operations,"generation":index.generation,"verified":true})
        );
        return;
    }
    let root = PathBuf::from(spec["root"].as_str().unwrap());
    std::fs::create_dir(&root).unwrap();
    begin(true);
    let data_clock = Instant::now();
    let mut samples = Vec::new();
    let mut last = Instant::now();
    let mut reported = Instant::now();
    let bytes=execute_read_plan(&drive,&plan,&root,&CancellationToken::default(),&mut |e|{
        if reported.elapsed()>=Duration::from_secs(5) {
            println!("RESTORE_PROGRESS={}",serde_json::json!({"seconds":data_clock.elapsed().as_secs_f64(),"path":e.tape_path,"read_bytes":e.bytes_read,"queued":e.buffer.as_ref().map(|b|b.used_bytes)}));
            reported=Instant::now();
        }
        if last.elapsed()>=Duration::from_millis(250) || e.telemetry.is_some() {
            samples.push(serde_json::json!({"seconds":data_clock.elapsed().as_secs_f64(),"path":e.tape_path,"read_bytes":e.bytes_read,"written_bytes":e.buffer.as_ref().map(|b|b.written_bytes),"queued":e.buffer.as_ref().map(|b|b.used_bytes),"waiting":e.buffer.as_ref().map(|b|b.tape_waiting),"telemetry":e.telemetry.as_ref().map(|t|serde_json::json!({"timestamp":t.timestamp,"temperature":t.temperature,"channel_rates":t.channel_rates,"counter_window":t.counter_window}))}));last=Instant::now();
        }
    }).unwrap();
    let seconds = data_clock.elapsed().as_secs_f64();
    let operations = end();
    verify_tape_hashes(&index, &plan, &root);
    println!(
        "RESTORE_RESULT={}",
        serde_json::json!({"kind":"tape","spec":spec,"bytes":bytes,"files":plan.files.len(),"extents":plan.extents.len(),"planning_seconds":planned,"restore_seconds":seconds,"operations":operations,"samples":samples,"generation":index.generation,"verified":true})
    );
}

#[test]
fn restore_fixture_is_append_only_and_has_four_eight_gib_groups() {
    let mut index = super::tests::empty_index();
    let old = index.root.clone();
    let plan = fixture_plan(&mut index, "/restore-fixture-unit", "2026-10-24T00:00:00Z").unwrap();
    assert_eq!(plan.total_bytes, 32 * 1024u64.pow(3));
    assert_eq!(plan.files.len(), 8192 + 2048 + 512 + 128);
    assert_eq!(
        &index.root.entries[..old.entries.len()],
        old.entries.as_slice()
    );
    for size in [1048576u64, 4194304, 16777216, 67108864] {
        let group = format!("/restore-fixture-unit/size-{size}");
        let read = plan_ltfs_read(&index, &[group]).unwrap();
        assert_eq!(read.payload_bytes, 8 * 1024u64.pow(3));
        assert_eq!(read.files.len() as u64, 8 * 1024u64.pow(3) / size);
    }
    assert!(fixture_plan(&mut index, "/restore-fixture-unit", "unused").is_err());
}
