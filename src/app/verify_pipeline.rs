//! Bounded, ordered SHA worker. Device access stays exclusively with the caller.
use super::CancellationToken;
use sha1::Sha1;
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(super) enum VerifyError {
    DigestMismatch {
        path: String,
        algorithm: &'static str,
        expected: String,
        actual: String,
    },
    Other(String),
}
impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DigestMismatch {
                path,
                algorithm,
                expected,
                actual,
            } => write!(
                f,
                "{path} {algorithm} 不匹配：期望 {expected}，实际 {actual}"
            ),
            Self::Other(error) => f.write_str(error),
        }
    }
}
impl From<String> for VerifyError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

enum Hasher {
    Sha1(Sha1),
    Sha256(Sha256),
}
impl Hasher {
    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha1(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
        }
    }
    fn finish_reset(&mut self) -> String {
        match self {
            Self::Sha1(h) => format!("{:x}", std::mem::take(h).finalize()),
            Self::Sha256(h) => format!("{:x}", std::mem::take(h).finalize()),
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::Sha1(_) => "SHA-1",
            Self::Sha256(_) => "SHA-256",
        }
    }
}

// At most 32 records (<=1MiB each), plus the extent cursor's shared-record cache.
const BUFFERS: usize = 32;
const POLL: Duration = Duration::from_millis(100);
enum Message {
    Data(Vec<u8>),
    Finish {
        path: String,
        length: u64,
        expected: String,
        reply: Sender<Result<(), VerifyError>>,
    },
}

pub(super) struct VerifyPipeline {
    input: Option<Sender<Message>>,
    recycled: Receiver<Vec<u8>>,
    worker: Option<JoinHandle<Result<(), String>>>,
    cancellation: CancellationToken,
    #[cfg(test)]
    wait_count: u64,
    #[cfg(test)]
    wait_seconds: f64,
}
impl VerifyPipeline {
    pub(super) fn spawn(cancellation: CancellationToken) -> Result<Self, String> {
        Self::spawn_hasher(cancellation, Hasher::Sha256(Sha256::new()))
    }
    pub(super) fn spawn_sha1(cancellation: CancellationToken) -> Result<Self, String> {
        Self::spawn_hasher(cancellation, Hasher::Sha1(Sha1::new()))
    }
    fn spawn_hasher(cancellation: CancellationToken, mut hasher: Hasher) -> Result<Self, String> {
        let (input, messages) = mpsc::channel();
        let (recycle, recycled) = mpsc::channel();
        // Tokens bound both allocation count and queued payload; no unbounded producer.
        for _ in 0..BUFFERS {
            recycle.send(Vec::new()).unwrap();
        }
        let token = cancellation.clone();
        let worker = std::thread::Builder::new()
            .name("tapecpy-verify".into())
            .spawn(move || {
                let mut bytes = 0u64;
                loop {
                    if token.is_cancelled() {
                        return Err("[cancelled]回读校验已取消".into());
                    }
                    match messages.recv_timeout(POLL) {
                        Ok(Message::Data(mut data)) => {
                            hasher.update(&data);
                            bytes += data.len() as u64;
                            data.clear();
                            if recycle.send(data).is_err() {
                                return Ok(());
                            }
                        }
                        Ok(Message::Finish {
                            path,
                            length,
                            expected,
                            reply,
                        }) => {
                            let actual = hasher.finish_reset();
                            let result = if bytes != length {
                                Err(VerifyError::Other(format!(
                                    "{path} 长度不匹配：index {length} 字节，回读 {bytes} 字节"
                                )))
                            } else if actual != expected {
                                Err(VerifyError::DigestMismatch {
                                    path,
                                    algorithm: hasher.name(),
                                    expected,
                                    actual,
                                })
                            } else {
                                Ok(())
                            };
                            bytes = 0;
                            let failed = result.is_err();
                            let _ = reply.send(result.clone());
                            if failed {
                                return result.map_err(|error| error.to_string());
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => (),
                        Err(RecvTimeoutError::Disconnected) => return Ok(()),
                    }
                }
            })
            .map_err(|e| format!("启动回读 SHA worker 失败: {e}"))?;
        Ok(Self {
            input: Some(input),
            recycled,
            worker: Some(worker),
            cancellation,
            #[cfg(test)]
            wait_count: 0,
            #[cfg(test)]
            wait_seconds: 0.0,
        })
    }
    fn check_cancel(&self) -> Result<(), String> {
        if self.cancellation.is_cancelled() {
            Err("[cancelled]回读校验已取消".into())
        } else {
            Ok(())
        }
    }
    fn worker_error(&mut self) -> String {
        self.input.take();
        match self.worker.take().map(|worker| worker.join()) {
            Some(Ok(Err(e))) => e,
            Some(Ok(Ok(()))) => "回读 SHA worker 意外结束".into(),
            Some(Err(_)) => "回读 SHA worker panic".into(),
            None => "回读 SHA worker 已结束".into(),
        }
    }
    pub(super) fn buffer(&mut self) -> Result<Vec<u8>, String> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return Err(self.worker_error());
        }
        if self.input.is_none() {
            return Err("回读 SHA worker 已结束".into());
        }
        #[cfg(test)]
        let waiting = {
            self.check_cancel()?;
            match self.recycled.try_recv() {
                Ok(buffer) => return Ok(buffer),
                Err(TryRecvError::Disconnected) => return Err(self.worker_error()),
                Err(TryRecvError::Empty) => {
                    self.wait_count += 1;
                    std::time::Instant::now()
                }
            }
        };
        loop {
            self.check_cancel()?;
            match self.recycled.recv_timeout(POLL) {
                Ok(buffer) => {
                    #[cfg(test)]
                    {
                        self.wait_seconds += waiting.elapsed().as_secs_f64();
                    }
                    return Ok(buffer);
                }
                Err(RecvTimeoutError::Timeout) => (),
                Err(RecvTimeoutError::Disconnected) => return Err(self.worker_error()),
            }
        }
    }
    pub(super) fn submit(&mut self, data: Vec<u8>) -> Result<(), String> {
        self.check_cancel()?;
        let Some(input) = &self.input else {
            return Err("回读 SHA worker 已结束".into());
        };
        if input.send(Message::Data(data)).is_err() {
            return Err(self.worker_error());
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn finish_file(
        &mut self,
        path: &str,
        length: u64,
        expected: &str,
    ) -> Result<(), String> {
        self.finish_file_checked(path, length, expected)
            .map_err(|error| error.to_string())
    }
    pub(super) fn finish_file_checked(
        &mut self,
        path: &str,
        length: u64,
        expected: &str,
    ) -> Result<(), VerifyError> {
        self.check_cancel()?;
        let (reply, response) = mpsc::channel();
        let Some(input) = &self.input else {
            return Err(VerifyError::Other("回读 SHA worker 已结束".into()));
        };
        if input
            .send(Message::Finish {
                path: path.into(),
                length,
                expected: expected.into(),
                reply,
            })
            .is_err()
        {
            return Err(self.worker_error().into());
        }
        loop {
            self.check_cancel()?;
            match response.recv_timeout(POLL) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) => (),
                Err(RecvTimeoutError::Disconnected) => return Err(self.worker_error().into()),
            }
        }
    }
}
impl Drop for VerifyPipeline {
    fn drop(&mut self) {
        self.input.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_mismatch_retains_actual_digest_and_closed_worker_is_fallible() {
        let mut pipeline = VerifyPipeline::spawn_sha1(CancellationToken::default()).unwrap();
        let error = pipeline
            .finish_file_checked("/empty", 0, "wrong")
            .unwrap_err();
        assert!(
            matches!(error,VerifyError::DigestMismatch {actual,..} if actual=="da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
        assert!(pipeline.worker_error().contains("SHA-1 不匹配"));
        assert!(pipeline.buffer().is_err());
        assert!(pipeline.submit(Vec::new()).is_err());
        assert!(pipeline.finish_file_checked("/empty", 0, "wrong").is_err());
    }

    #[test]
    fn sha1_known_vectors_reset_and_mismatch_use_same_worker() {
        let mut pipeline = VerifyPipeline::spawn_sha1(CancellationToken::default()).unwrap();
        let mut buffer = pipeline.buffer().unwrap();
        buffer.extend_from_slice(b"abc");
        pipeline.submit(buffer).unwrap();
        pipeline
            .finish_file("abc", 3, "a9993e364706816aba3e25717850c26c9cd0d89d")
            .unwrap();
        pipeline
            .finish_file("empty", 0, "da39a3ee5e6b4b0d3255bfef95601890afd80709")
            .unwrap();
        for _ in 0..1000 {
            let mut buffer = pipeline.buffer().unwrap();
            buffer.extend(std::iter::repeat_n(b'a', 1000));
            pipeline.submit(buffer).unwrap();
        }
        pipeline
            .finish_file(
                "million-a",
                1_000_000,
                "34aa973cd4c4daa4f61eeb2bdbad27316534016f",
            )
            .unwrap();
        assert!(
            pipeline
                .finish_file("bad", 0, "wrong")
                .unwrap_err()
                .contains("SHA-1 不匹配")
        );
    }

    #[test]
    fn ordered_many_buffers_and_file_barriers_including_empty_file() {
        let mut pipeline = VerifyPipeline::spawn(CancellationToken::default()).unwrap();
        for size in [0, 1, 100, 65] {
            let mut expected = Sha256::new();
            let mut bytes = 0;
            for n in 0..size {
                let mut buffer = pipeline.buffer().unwrap();
                buffer.extend(std::iter::repeat_n(n as u8, 1024 + n));
                expected.update(&buffer);
                bytes += buffer.len() as u64;
                pipeline.submit(buffer).unwrap();
            }
            pipeline
                .finish_file("file", bytes, &format!("{:x}", expected.finalize()))
                .unwrap();
        }
    }

    #[test]
    fn length_and_digest_failure_are_not_success() {
        for (length, expected, message) in [
            (1, format!("{:x}", Sha256::digest([])), "长度不匹配"),
            (0, "wrong".into(), "SHA-256 不匹配"),
        ] {
            let mut pipeline = VerifyPipeline::spawn(CancellationToken::default()).unwrap();
            assert!(
                pipeline
                    .finish_file("/bad", length, &expected)
                    .unwrap_err()
                    .contains(message)
            );
        }
    }

    #[test]
    fn bounded_pool_wait_is_cancelled_and_worker_is_joined() {
        let token = CancellationToken::default();
        let mut pipeline = VerifyPipeline::spawn(token.clone()).unwrap();
        let held: Vec<_> = (0..BUFFERS).map(|_| pipeline.buffer().unwrap()).collect();
        // All allocation tokens held: another allocation must wait rather than grow.
        let (started, ready) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            started.send(()).unwrap();
            assert!(pipeline.buffer().unwrap_err().contains("[cancelled]"));
            finished.send(()).unwrap();
        });
        ready.recv().unwrap();
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(20)),
            Err(RecvTimeoutError::Timeout)
        ));
        token.request_cancel();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        waiter.join().unwrap();
        drop(held);
    }

    #[test]
    fn tape_error_preserves_original_error_and_joins_pending_hashes() {
        use super::super::{
            Extent, ExtentReadCursor, ExtentTape, FileEntry, FileHash, verify_file_hash,
        };
        struct BrokenTape(usize);
        impl ExtentTape for BrokenTape {
            fn locate_extent(&mut self, _: u8, _: u64) -> Result<(), String> {
                Ok(())
            }
            fn read_extent_record(&mut self, mut buffer: Vec<u8>) -> Result<Vec<u8>, String> {
                self.0 += 1;
                if self.0 == 3 {
                    return Err("injected READ failure".into());
                }
                buffer.resize(512, 42);
                Ok(buffer)
            }
        }
        let file = FileEntry {
            length: 2048,
            extents: vec![Extent {
                partition: 1,
                start_block: 0,
                byte_offset: 0,
                byte_count: 2048,
                file_offset: 0,
            }],
            ..Default::default()
        };
        let mut pipeline = VerifyPipeline::spawn(CancellationToken::default()).unwrap();
        let result = verify_file_hash(
            &mut BrokenTape(0),
            &file,
            &FileHash {
                path: "/error".into(),
                sha256: String::new(),
            },
            &mut ExtentReadCursor::default(),
            &mut pipeline,
        );
        assert_eq!(result.unwrap_err(), "injected READ failure");
        drop(pipeline);
    }

    #[test]
    fn worker_panic_is_reported_and_drop_does_not_panic() {
        let mut pipeline = VerifyPipeline::spawn(CancellationToken::default()).unwrap();
        pipeline.input.take();
        pipeline.worker.take().unwrap().join().unwrap().unwrap();
        pipeline.worker = Some(std::thread::spawn(|| panic!("injected worker failure")));
        assert!(pipeline.worker_error().contains("panic"));
    }
}

#[cfg(test)]
mod hardware {
    use super::super::*;
    use std::time::Instant;

    #[test]
    #[ignore = "explicit read-only serial/pipelined verify A/B on an existing tape file"]
    fn verify_ab() {
        let spec: serde_json::Value =
            serde_json::from_str(&std::env::var("TAPECPY_VERIFY_AB").unwrap()).unwrap();
        let drives = discover_drives().unwrap();
        let drive = select_drive(&drives, Some("/dev/sg0")).unwrap();
        let _lease = device::lease::DeviceLease::try_acquire(
            &drive.serial,
            device::lease::LeaseOwner::new("hardware-test", "verify-ab"),
        )
        .unwrap();
        let volume = inspect_volume(drive).unwrap();
        let index = volume.index.unwrap();
        let mut file = index
            .find_file(spec["path"].as_str().unwrap())
            .unwrap()
            .clone();
        let bytes = spec["bytes"].as_u64().unwrap();
        assert_eq!(file.extents.len(), 1);
        assert!(bytes > 0 && bytes <= file.length && bytes <= file.extents[0].byte_count);
        file.length = bytes;
        file.extents[0].byte_count = bytes;
        let expected = FileHash {
            path: spec["path"].as_str().unwrap().into(),
            sha256: spec["sha256"].as_str().unwrap().into(),
        };
        struct TimedTape {
            tape: TapeSession,
            first: Option<Instant>,
            reads: usize,
            io_seconds: f64,
            previous_end: Option<Instant>,
            gap_seconds: f64,
            max_gap_seconds: f64,
            gaps_over_1ms: usize,
            payload: u64,
            last_sample: Instant,
            sample_payload: u64,
            mode: String,
        }
        impl ExtentTape for TimedTape {
            fn locate_extent(&mut self, p: u8, b: u64) -> Result<(), String> {
                self.tape.locate_extent(p, b)
            }
            fn read_extent_record(&mut self, buffer: Vec<u8>) -> Result<Vec<u8>, String> {
                let start = Instant::now();
                if self.first.is_none() {
                    self.last_sample = start;
                }
                self.first.get_or_insert(start);
                if let Some(end) = self.previous_end {
                    let gap = start.duration_since(end).as_secs_f64();
                    self.gap_seconds += gap;
                    self.max_gap_seconds = self.max_gap_seconds.max(gap);
                    self.gaps_over_1ms += usize::from(gap > 0.001);
                }
                let result = self.tape.read_extent_record(buffer);
                self.io_seconds += start.elapsed().as_secs_f64();
                self.reads += 1;
                self.previous_end = Some(Instant::now());
                if let Ok(data) = &result {
                    self.payload += data.len() as u64;
                }
                if self.last_sample.elapsed().as_secs_f64() >= 1.0 {
                    println!(
                        "VERIFY_STREAM_SAMPLE={}",
                        serde_json::json!({"mode":self.mode,"seconds":self.first.unwrap().elapsed().as_secs_f64(),"bytes":self.payload,"interval_MB_s":(self.payload-self.sample_payload) as f64/self.last_sample.elapsed().as_secs_f64()/1e6})
                    );
                    self.last_sample = Instant::now();
                    self.sample_payload = self.payload;
                }
                result
            }
        }
        // AB/BA, same file/prefix; fresh cursor forces equivalent locate each time.
        let modes: Vec<String> = spec["modes"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_else(|| {
                ["serial", "pipeline", "pipeline", "serial"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            });
        for mode in &modes {
            assert!(["read", "serial", "pipeline"].contains(&mode.as_str()));
            let mut tape = TimedTape {
                tape: TapeSession::open(&drive.sg_path).unwrap(),
                first: None,
                reads: 0,
                io_seconds: 0.0,
                previous_end: None,
                gap_seconds: 0.0,
                max_gap_seconds: 0.0,
                gaps_over_1ms: 0,
                payload: 0,
                last_sample: Instant::now(),
                sample_payload: 0,
                mode: mode.clone(),
            };
            let start = Instant::now();
            let mut cursor = ExtentReadCursor::default();
            let mut cpu_time = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            unsafe {
                libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut cpu_time);
            }
            let cpu_start = cpu_time.tv_sec as f64 + cpu_time.tv_nsec as f64 / 1e9;
            let mut pool_wait = (0, 0.0);
            if mode == "pipeline" {
                let mut pipeline =
                    super::VerifyPipeline::spawn(CancellationToken::default()).unwrap();
                verify_file_hash(&mut tape, &file, &expected, &mut cursor, &mut pipeline).unwrap();
                pool_wait = (pipeline.wait_count, pipeline.wait_seconds);
            } else {
                let mut hash = Sha256::new();
                let mut buffer = Vec::new();
                let extent = &file.extents[0];
                let mut remaining = bytes;
                let mut block = extent.start_block;
                let mut offset = extent.byte_offset;
                while remaining > 0 {
                    buffer = cursor
                        .chunk(
                            &mut tape,
                            extent.partition,
                            block,
                            offset,
                            remaining,
                            buffer,
                        )
                        .unwrap();
                    if mode != "read" {
                        hash.update(&buffer);
                    }
                    remaining -= buffer.len() as u64;
                    block += 1;
                    offset = 0;
                }
                if mode != "read" {
                    assert_eq!(format!("{:x}", hash.finalize()), expected.sha256);
                }
            }
            unsafe {
                libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut cpu_time);
            }
            let cpu_seconds = cpu_time.tv_sec as f64 + cpu_time.tv_nsec as f64 / 1e9 - cpu_start;
            let streaming = tape.first.unwrap().elapsed().as_secs_f64();
            println!(
                "VERIFY_AB_RESULT={}",
                serde_json::json!({"mode":mode,"bytes":bytes,"wall_seconds":start.elapsed().as_secs_f64(),"stream_seconds":streaming,"MB_s":bytes as f64/streaming/1e6,"cpu_seconds":cpu_seconds,"read_calls":tape.reads,"read_io_seconds":tape.io_seconds,"verified":if mode == "read" { None } else { Some(true) },"pool_wait_count":pool_wait.0,"pool_wait_seconds":pool_wait.1,"read_gap_seconds":tape.gap_seconds,"max_read_gap_seconds":tape.max_gap_seconds,"read_gaps_over_1ms":tape.gaps_over_1ms})
            );
        }
    }
}
