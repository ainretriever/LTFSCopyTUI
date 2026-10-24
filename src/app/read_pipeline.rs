//! Bounded host-output batches, file lifecycle and restore completion barriers.
//! Only the tape-owner thread produces batches; this worker never accesses a device.
#[cfg(test)]
use super::restore_tests;
use super::{
    CancellationToken, READ_OUTPUT_HANDLE_CAPACITY, ReadBufferSample, ReadPlan, create_file_at,
    ensure_directory_at, open_destination_directory, open_existing_directory_at, open_file_at,
};
use std::collections::BTreeSet;
use std::io;
use std::os::fd::{AsRawFd, IntoRawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// Linux close must not be retried after EINTR: the fd may already be released.
fn close_read_output(file: std::fs::File, path: &Path) -> Result<(), String> {
    let fd = file.into_raw_fd();
    // SAFETY: into_raw_fd transfers unique ownership; close is attempted exactly once.
    if unsafe { libc::close(fd) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "关闭恢复文件 {} 失败: {}",
            path.display(),
            io::Error::last_os_error()
        ))
    }
}

pub(super) fn sync_read_output(file: &std::fs::File, path: &Path) -> Result<(), String> {
    restore_probe!("file_sync");
    file.sync_all()
        .map_err(|error| format!("同步恢复文件 {} 失败: {error}", path.display()))
}

pub(super) struct ReadOutputFiles {
    pub(super) destination: PathBuf,
    pub(super) sync_anchor: std::fs::File,
    pub(super) handles: Vec<Option<std::fs::File>>,
    pub(super) extents_remaining: Vec<usize>,
    pub(super) cached_handles: usize,
    pub(super) directories_to_sync: Vec<PathBuf>,
}

impl ReadOutputFiles {
    pub(super) fn prepare(plan: &ReadPlan, destination: &Path) -> Result<Self, String> {
        restore_probe!("prepare");
        let sync_anchor = open_destination_directory(destination).map_err(|error| {
            format!(
                "安全打开 Read destination {} 失败: {error}",
                destination.display()
            )
        })?;
        let mut directories = plan.directories.iter().cloned().collect::<BTreeSet<_>>();
        for file in &plan.files {
            if let Some(parent) = file.relative_path.parent()
                && !parent.as_os_str().is_empty()
            {
                directories.insert(parent.to_path_buf());
            }
        }
        // Include intermediate parents, then sync child directories before parents.
        let mut all_directories = BTreeSet::new();
        for directory in &directories {
            for ancestor in directory.ancestors().filter(|p| !p.as_os_str().is_empty()) {
                all_directories.insert(ancestor.to_path_buf());
            }
        }
        let mut directories_to_sync: Vec<_> = all_directories.into_iter().collect();
        directories_to_sync.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        for directory in directories {
            restore_probe!("mkdir");
            ensure_directory_at(&sync_anchor, &directory)?;
        }

        let mut extents_remaining = vec![0usize; plan.files.len()];
        let mut retain = BTreeSet::new();
        for extent in &plan.extents {
            extents_remaining[extent.file_index] += 1;
            if retain.len() < READ_OUTPUT_HANDLE_CAPACITY {
                retain.insert(extent.file_index);
            }
        }
        let requires_preallocation = preallocation_requirements(plan);

        let mut handles = (0..plan.files.len()).map(|_| None).collect::<Vec<_>>();
        let mut cached_handles = 0;
        for (index, file) in plan.files.iter().enumerate() {
            let target = destination.join(&file.relative_path);
            let output = {
                restore_probe!("create");
                create_file_at(&sync_anchor, &file.relative_path)
            }
            .map_err(|error| {
                format!(
                    "安全创建 Read destination {} 失败: {error}",
                    target.display()
                )
            })?;
            if requires_preallocation[index] {
                restore_probe!("set_len");
                output.set_len(file.length).map_err(|error| {
                    format!("设置恢复文件长度 {} 失败: {error}", target.display())
                })?;
            }
            if extents_remaining[index] == 0 {
                sync_read_output(&output, &target)?;
                close_read_output(output, &target)?;
            } else if retain.contains(&index) {
                handles[index] = Some(output);
                cached_handles += 1;
            } else {
                restore_probe!("close_prepare");
                close_read_output(output, &target)?;
            }
        }
        Ok(Self {
            destination: destination.to_path_buf(),
            sync_anchor,
            handles,
            extents_remaining,
            cached_handles,
            directories_to_sync,
        })
    }

    pub(super) fn open_extent(
        &mut self,
        plan: &ReadPlan,
        file_index: usize,
    ) -> Result<std::fs::File, String> {
        restore_probe!("open_extent");
        if let Some(output) = self.handles[file_index].take() {
            self.cached_handles -= 1;
            return Ok(output);
        }
        let target = self.destination.join(&plan.files[file_index].relative_path);
        restore_probe!("reopen");
        open_file_at(&self.sync_anchor, &plan.files[file_index].relative_path)
            .map_err(|error| format!("安全重新打开恢复文件 {} 失败: {error}", target.display()))
    }

    pub(super) fn finish_extent(
        &mut self,
        plan: &ReadPlan,
        file_index: usize,
        output: std::fs::File,
    ) -> Result<bool, String> {
        let target = self.destination.join(&plan.files[file_index].relative_path);
        self.extents_remaining[file_index] -= 1;
        if self.extents_remaining[file_index] == 0 {
            sync_read_output(&output, &target)?;
            {
                restore_probe!("close_final");
                close_read_output(output, &target)?;
            }
            return Ok(true);
        }
        if self.cached_handles < READ_OUTPUT_HANDLE_CAPACITY {
            self.handles[file_index] = Some(output);
            self.cached_handles += 1;
        } else {
            close_read_output(output, &target)?;
        }
        Ok(false)
    }

    pub(super) fn sync_filesystem(&self) -> Result<(), String> {
        restore_probe!("syncfs");
        // File fsyncs happen on the still-open final handles. syncfs alone does not
        // request SMB FLUSH for files already closed. Persist namespace separately.
        for directory in &self.directories_to_sync {
            let handle = open_existing_directory_at(&self.sync_anchor, directory)
                .map_err(|error| format!("打开待同步目录 {} 失败: {error}", directory.display()))?;
            handle
                .sync_all()
                .map_err(|error| format!("同步恢复目录 {} 失败: {error}", directory.display()))?;
        }
        self.sync_anchor
            .sync_all()
            .map_err(|error| format!("同步恢复根目录失败: {error}"))?;
        // SAFETY: sync_anchor remains open for the duration of this call.
        if unsafe { libc::syncfs(self.sync_anchor.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(format!(
                "同步 Read destination filesystem {} 失败: {}",
                self.destination.display(),
                std::io::Error::last_os_error()
            ))
        }
    }
}

fn preallocation_requirements(plan: &ReadPlan) -> Vec<bool> {
    let mut file_extents = vec![Vec::new(); plan.files.len()];
    for extent in &plan.extents {
        file_extents[extent.file_index].push((extent.file_offset, extent.byte_count));
    }
    file_extents
        .into_iter()
        .zip(&plan.files)
        .map(|(mut extents, file)| {
            extents.sort_unstable_by_key(|(offset, _)| *offset);
            let mut expected_offset = 0u64;
            for (offset, byte_count) in extents {
                if offset != expected_offset {
                    return true;
                }
                expected_offset = match expected_offset.checked_add(byte_count) {
                    Some(offset) => offset,
                    None => return true,
                };
            }
            expected_offset != file.length
        })
        .collect()
}

pub(super) enum ReadPipelineMessage {
    ExtentStart { file_index: usize, file_offset: u64 },
    ExtentEnd,
    Finish,
}

// Payload slabs are independent of tape records and file/extent boundaries.
// A slab has a bounded descriptor array as well as a bounded payload allocation.
pub(super) const READ_BATCH_BYTES: usize = crate::device::tape::READ_BUF_LEN;
pub(super) const READ_BATCH_ENTRIES: usize = 4096;
pub(super) const READ_WAIT_PULSE: Duration = Duration::from_millis(100);

pub(super) enum ReadBatchEntry {
    ExtentStart { file_index: usize, file_offset: u64 },
    Data { start: usize, length: usize },
    ExtentEnd,
    Finish,
}

pub(super) struct ReadBatch {
    pub(super) data: Vec<u8>,
    pub(super) entries: Vec<ReadBatchEntry>,
}
impl ReadBatch {
    pub(super) fn new() -> Self {
        Self {
            data: Vec::with_capacity(READ_BATCH_BYTES),
            entries: Vec::new(),
        }
    }
    pub(super) fn clear(&mut self) {
        self.data.clear();
        self.entries.clear();
    }
}

#[derive(Default)]
pub(super) struct ReadPipelineMetrics {
    pub(super) written_bytes: std::sync::atomic::AtomicU64,
    pub(super) queued_bytes: std::sync::atomic::AtomicU64,
    pub(super) queued_records: std::sync::atomic::AtomicU64,
    pub(super) tape_waiting: std::sync::atomic::AtomicBool,
    pub(super) destination_waiting: std::sync::atomic::AtomicBool,
    pub(super) wait_nanoseconds: std::sync::atomic::AtomicU64,
    pub(super) wait_events: std::sync::atomic::AtomicU64,
    pub(super) files_completed: std::sync::atomic::AtomicU64,
}

pub(super) struct ReadBatchProducer {
    pub(super) pending: Option<ReadBatch>,
    pub(super) allocated: usize,
    pub(super) scratch: Option<Vec<u8>>,
}

pub(super) struct ReadDestinationPipeline {
    pub(super) sender: Option<std::sync::mpsc::Sender<ReadBatch>>,
    pub(super) recycle: std::sync::mpsc::Receiver<ReadBatch>,
    pub(super) producer: std::cell::RefCell<ReadBatchProducer>,
    pub(super) cancellation: CancellationToken,
    pub(super) metrics: std::sync::Arc<ReadPipelineMetrics>,
    pub(super) capacity_bytes: u64,
    pub(super) batch_capacity: usize,
    pub(super) completion: std::sync::mpsc::Receiver<()>,
    pub(super) worker: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl ReadDestinationPipeline {
    pub(super) fn spawn(
        plan: ReadPlan,
        outputs: ReadOutputFiles,
        cancellation: CancellationToken,
        capacity_bytes: usize,
    ) -> Result<Self, String> {
        let batch_capacity = capacity_bytes.div_ceil(READ_BATCH_BYTES).max(1);
        let actual_capacity = batch_capacity
            .checked_mul(READ_BATCH_BYTES)
            .ok_or_else(|| "read pipeline capacity 溢出".to_string())?;
        // The channel is logically bounded by slab ownership: at most batch_capacity
        // slabs exist across producer, channel, writer and recycle channel together.
        let (sender, messages) = std::sync::mpsc::channel();
        let (completion_tx, completion) = std::sync::mpsc::channel::<()>();
        let (recycle_tx, recycle) = std::sync::mpsc::channel();
        let metrics = std::sync::Arc::new(ReadPipelineMetrics::default());
        metrics.files_completed.store(
            outputs
                .extents_remaining
                .iter()
                .filter(|count| **count == 0)
                .count() as u64,
            std::sync::atomic::Ordering::Release,
        );
        let worker_metrics = metrics.clone();
        let worker_cancellation = cancellation.clone();
        let worker = std::thread::Builder::new()
            .name("tapecpy-destination-writer".into())
            .spawn(move || {
                let result = write_read_pipeline(
                    &plan,
                    outputs,
                    worker_cancellation,
                    &messages,
                    &recycle_tx,
                    &worker_metrics,
                );
                drop(completion_tx);
                result
            })
            .map_err(|error| format!("启动 Read destination writer 失败: {error}"))?;
        Ok(Self {
            sender: Some(sender),
            recycle,
            producer: std::cell::RefCell::new(ReadBatchProducer {
                pending: None,
                allocated: 0,
                scratch: None,
            }),
            cancellation,
            metrics,
            capacity_bytes: actual_capacity as u64,
            batch_capacity,
            completion,
            worker: Some(worker),
        })
    }

    fn acquire_batch(
        &self,
        on_wait: &mut dyn FnMut(ReadBufferSample),
    ) -> Result<ReadBatch, String> {
        use std::sync::atomic::Ordering;
        if self.cancellation.is_cancelled() {
            return Err("[cancelled]tape reader 在缓冲边界停止".into());
        }
        match self.recycle.try_recv() {
            Ok(mut batch) => {
                batch.clear();
                return Ok(batch);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("Read destination writer 已关闭 buffer recycle channel".into());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        {
            let mut producer = self.producer.borrow_mut();
            if producer.allocated < self.batch_capacity {
                producer.allocated += 1;
                return Ok(ReadBatch::new());
            }
        }
        self.metrics.tape_waiting.store(true, Ordering::Release);
        self.metrics.wait_events.fetch_add(1, Ordering::AcqRel);
        let mut tick = Instant::now();
        on_wait(self.snapshot());
        let result = loop {
            if self.cancellation.is_cancelled() {
                break Err("[cancelled]tape reader 等待恢复缓冲时停止".into());
            }
            let received = self.recycle.recv_timeout(READ_WAIT_PULSE);
            self.metrics.wait_nanoseconds.fetch_add(
                tick.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                Ordering::AcqRel,
            );
            tick = Instant::now();
            match received {
                Ok(mut batch) => {
                    batch.clear();
                    break Ok(batch);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => on_wait(self.snapshot()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break Err("Read destination writer 已关闭 buffer recycle channel".into());
                }
            }
        };
        self.metrics.tape_waiting.store(false, Ordering::Release);
        // Publish the cleared state too, even if no further tape record is read.
        on_wait(self.snapshot());
        result
    }

    fn ensure_batch(&self, on_wait: &mut dyn FnMut(ReadBufferSample)) -> Result<(), String> {
        if self.cancellation.is_cancelled() {
            return Err("[cancelled]tape reader 在缓冲边界停止".into());
        }
        if self.producer.borrow().pending.is_none() {
            let batch = self.acquire_batch(on_wait)?;
            let mut producer = self.producer.borrow_mut();
            producer.pending = Some(batch);
        }
        Ok(())
    }

    pub(super) fn flush_batch(&self) -> Result<(), String> {
        let batch = {
            let mut producer = self.producer.borrow_mut();
            producer.pending.take()
        };
        if let Some(batch) = batch {
            self.sender
                .as_ref()
                .expect("read pipeline sender exists")
                .send(batch)
                .map_err(|_| "Read destination writer 已停止".to_string())?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn send(&self, message: ReadPipelineMessage) -> Result<(), String> {
        self.send_observed(message, &mut |_| {})
    }

    pub(super) fn send_observed(
        &self,
        message: ReadPipelineMessage,
        on_wait: &mut dyn FnMut(ReadBufferSample),
    ) -> Result<(), String> {
        restore_probe!("queue_send");
        let entry = match message {
            ReadPipelineMessage::ExtentStart {
                file_index,
                file_offset,
            } => ReadBatchEntry::ExtentStart {
                file_index,
                file_offset,
            },
            ReadPipelineMessage::ExtentEnd => ReadBatchEntry::ExtentEnd,
            ReadPipelineMessage::Finish => ReadBatchEntry::Finish,
        };
        let finish = matches!(entry, ReadBatchEntry::Finish);
        self.ensure_batch(on_wait)?;
        let full = {
            let mut producer = self.producer.borrow_mut();
            let batch = producer.pending.as_mut().unwrap();
            batch.entries.push(entry);
            batch.entries.len() == READ_BATCH_ENTRIES
        };
        if full || finish {
            self.flush_batch()?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn send_data(&self, data: Vec<u8>) -> Result<(), String> {
        self.send_data_observed(data, &mut |_| {})
    }

    pub(super) fn send_data_observed(
        &self,
        data: Vec<u8>,
        on_wait: &mut dyn FnMut(ReadBufferSample),
    ) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        let mut offset = 0;
        while offset < data.len() {
            self.ensure_batch(on_wait)?;
            let count = {
                let mut producer = self.producer.borrow_mut();
                let batch = producer.pending.as_mut().unwrap();
                let start = batch.data.len();
                let count = (READ_BATCH_BYTES - start).min(data.len() - offset);
                batch.data.extend_from_slice(&data[offset..offset + count]);
                batch.entries.push(ReadBatchEntry::Data {
                    start,
                    length: count,
                });
                self.metrics
                    .queued_bytes
                    .fetch_add(count as u64, Ordering::AcqRel);
                self.metrics.queued_records.fetch_add(1, Ordering::AcqRel);
                count
            };
            offset += count;
            let full = self
                .producer
                .borrow()
                .pending
                .as_ref()
                .is_some_and(|batch| {
                    batch.data.len() == READ_BATCH_BYTES
                        || batch.entries.len() == READ_BATCH_ENTRIES
                });
            if full {
                self.flush_batch()?;
            }
        }
        self.producer.borrow_mut().scratch = Some(data);
        Ok(())
    }

    pub(super) fn acquire_buffer(&self, allocated: &mut usize) -> Result<Vec<u8>, String> {
        restore_probe!("acquire_buffer");
        if self.cancellation.is_cancelled() {
            return Err("[cancelled]tape reader 在 record 边界停止".into());
        }
        Ok(self
            .producer
            .borrow_mut()
            .scratch
            .take()
            .unwrap_or_else(|| {
                *allocated += 1;
                Vec::with_capacity(crate::device::tape::READ_BUF_LEN)
            }))
    }

    pub(super) fn snapshot(&self) -> ReadBufferSample {
        use std::sync::atomic::Ordering;
        ReadBufferSample {
            written_bytes: self.metrics.written_bytes.load(Ordering::Acquire),
            used_bytes: self.metrics.queued_bytes.load(Ordering::Acquire),
            capacity_bytes: self.capacity_bytes,
            tape_waiting: self.metrics.tape_waiting.load(Ordering::Acquire),
            destination_waiting: self.metrics.destination_waiting.load(Ordering::Acquire),
            wait_nanoseconds: self.metrics.wait_nanoseconds.load(Ordering::Acquire),
            wait_events: self.metrics.wait_events.load(Ordering::Acquire),
        }
    }

    pub(super) fn files_completed(&self) -> usize {
        self.metrics
            .files_completed
            .load(std::sync::atomic::Ordering::Acquire) as usize
    }

    #[cfg(test)]
    pub(super) fn finish(self) -> Result<(), String> {
        self.finish_observed(&mut |_| {})
    }

    pub(super) fn finish_observed(
        mut self,
        on_wait: &mut dyn FnMut(ReadBufferSample),
    ) -> Result<(), String> {
        let send_result = self.send_observed(ReadPipelineMessage::Finish, on_wait);
        self.sender.take();
        while matches!(
            self.completion.recv_timeout(READ_WAIT_PULSE),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
            on_wait(self.snapshot());
        }
        let worker_result = self
            .worker
            .take()
            .expect("read pipeline worker exists")
            .join()
            .map_err(|_| "Read destination writer thread panic".to_string())?;
        on_wait(self.snapshot());
        worker_result.and(send_result)
    }

    pub(super) fn abort(mut self) -> Result<(), String> {
        self.sender.take();
        self.producer.borrow_mut().pending.take();
        self.worker
            .take()
            .expect("read pipeline worker exists")
            .join()
            .map_err(|_| "Read destination writer thread panic".to_string())?
    }
}

pub(super) fn write_read_pipeline(
    plan: &ReadPlan,
    mut outputs: ReadOutputFiles,
    cancellation: CancellationToken,
    messages: &std::sync::mpsc::Receiver<ReadBatch>,
    recycle: &std::sync::mpsc::Sender<ReadBatch>,
    metrics: &ReadPipelineMetrics,
) -> Result<(), String> {
    #[cfg(test)]
    restore_tests::pause_destination();
    let mut active: Option<(usize, std::fs::File)> = None;
    loop {
        metrics
            .destination_waiting
            .store(true, std::sync::atomic::Ordering::Release);
        let received = messages.recv();
        metrics
            .destination_waiting
            .store(false, std::sync::atomic::Ordering::Release);
        let mut batch =
            received.map_err(|_| "tape reader 在 Read pipeline 完成前停止".to_string())?;
        for entry in &batch.entries {
            if cancellation.is_cancelled() {
                return Err("[cancelled]destination writer 在 record 边界停止".into());
            }
            match *entry {
                ReadBatchEntry::ExtentStart {
                    file_index,
                    file_offset,
                } => {
                    if active.is_some() {
                        return Err("Read pipeline 在前一个 extent 结束前开始了新 extent".into());
                    }
                    let mut output = outputs.open_extent(plan, file_index)?;
                    let seek_result = {
                        restore_probe!("seek");
                        std::io::Seek::seek(&mut output, std::io::SeekFrom::Start(file_offset))
                    };
                    seek_result.map_err(|error| {
                        format!(
                            "定位恢复文件 {} 失败: {error}",
                            outputs
                                .destination
                                .join(&plan.files[file_index].relative_path)
                                .display()
                        )
                    })?;
                    active = Some((file_index, output));
                }
                ReadBatchEntry::Data { start, length } => {
                    let data = &batch.data[start..start + length];
                    let length = length as u64;
                    let (_, output) = active
                        .as_mut()
                        .ok_or_else(|| "Read pipeline 收到 extent 外的数据".to_string())?;
                    {
                        restore_probe!("write");
                        std::io::Write::write_all(output, data)
                    }
                    .map_err(|error| format!("写入 Read destination 失败: {error}"))?;
                    metrics
                        .written_bytes
                        .fetch_add(length, std::sync::atomic::Ordering::AcqRel);
                    metrics
                        .queued_bytes
                        .fetch_sub(length, std::sync::atomic::Ordering::AcqRel);
                    metrics
                        .queued_records
                        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                }
                ReadBatchEntry::ExtentEnd => {
                    let (file_index, output) = active
                        .take()
                        .ok_or_else(|| "Read pipeline 收到重复的 extent end".to_string())?;
                    if outputs.finish_extent(plan, file_index, output)? {
                        metrics
                            .files_completed
                            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    }
                }
                ReadBatchEntry::Finish => {
                    if active.is_some() {
                        return Err("Read pipeline 在 extent 未结束时请求完成".into());
                    }
                    outputs.sync_filesystem()?;
                    return Ok(());
                }
            }
        }
        batch.clear();
        let _ = recycle.send(batch);
    }
}
