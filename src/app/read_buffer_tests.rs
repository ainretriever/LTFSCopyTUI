use super::*;
use std::sync::{Arc, mpsc};

// A real destination worker held behind a per-instance gate, no global sleep hook.
fn paused(capacity: usize, name: &str) -> (ReadDestinationPipeline, mpsc::Sender<()>, PathBuf) {
    let root = std::env::temp_dir().join(format!("tapecpy-batch-{name}-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let plan = restore_tests::host_plan(1048576, 1, 1048576, false);
    let outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    let (sender, messages) = mpsc::channel();
    let (recycle_tx, recycle) = mpsc::channel();
    let (completion_tx, completion) = mpsc::channel::<()>();
    let (resume, gate) = mpsc::channel();
    let metrics = Arc::new(ReadPipelineMetrics::default());
    let worker_metrics = metrics.clone();
    let cancellation = CancellationToken::default();
    let worker_cancel = cancellation.clone();
    let worker = std::thread::spawn(move || {
        gate.recv().unwrap();
        let result = write_read_pipeline(
            &plan,
            outputs,
            worker_cancel,
            &messages,
            &recycle_tx,
            &worker_metrics,
        );
        drop(completion_tx);
        result
    });
    (
        ReadDestinationPipeline {
            sender: Some(sender),
            recycle,
            producer: std::cell::RefCell::new(ReadBatchProducer {
                pending: None,
                allocated: 0,
                scratch: None,
            }),
            cancellation,
            metrics,
            capacity_bytes: capacity as u64,
            batch_capacity: capacity / READ_BATCH_BYTES,
            completion,
            worker: Some(worker),
        },
        resume,
        root,
    )
}

#[test]
fn compact_slabs_use_payload_budget_and_publish_wait_while_blocked() {
    for size in [1024, 4096, 65536, 524288] {
        let (pipeline, resume, root) = paused(4 * READ_BATCH_BYTES, &format!("capacity-{size}"));
        let cancellation = pipeline.cancellation.clone();
        let mut samples = Vec::new();
        let mut observe = |sample: ReadBufferSample| {
            samples.push(sample);
            if sample.wait_nanoseconds >= 150_000_000 {
                cancellation.request_cancel();
            }
        };
        let mut allocated = 0;
        let result = (|| -> Result<(), String> {
            loop {
                pipeline.send_observed(
                    ReadPipelineMessage::ExtentStart {
                        file_index: 0,
                        file_offset: 0,
                    },
                    &mut observe,
                )?;
                let mut data = pipeline.acquire_buffer(&mut allocated)?;
                data.resize(size, 7);
                pipeline.send_data_observed(data, &mut observe)?;
                pipeline.send_observed(ReadPipelineMessage::ExtentEnd, &mut observe)?;
            }
        })();
        assert!(result.unwrap_err().contains("cancelled"));
        let last = pipeline.snapshot();
        assert_eq!(last.used_bytes, last.capacity_bytes, "size={size}");
        assert_eq!(pipeline.producer.borrow().allocated, 4);
        assert_eq!(allocated, 1, "record scratch should be reused");
        assert!(samples.iter().filter(|s| s.tape_waiting).count() >= 2);
        assert!(last.wait_nanoseconds >= 150_000_000);
        assert_eq!(last.wait_events, 1);
        assert!(!samples.last().unwrap().tape_waiting);
        resume.send(()).unwrap();
        assert!(pipeline.abort().unwrap_err().contains("cancelled"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn resume_clears_wait_and_reuses_slabs_without_changing_order() {
    let (pipeline, resume, root) = paused(READ_BATCH_BYTES, "resume");
    pipeline
        .send(ReadPipelineMessage::ExtentStart {
            file_index: 0,
            file_offset: 0,
        })
        .unwrap();
    pipeline.send_data(vec![3; READ_BATCH_BYTES]).unwrap();
    let mut resume = Some(resume);
    let mut saw_wait = false;
    pipeline
        .send_observed(ReadPipelineMessage::ExtentEnd, &mut |sample| {
            if sample.tape_waiting {
                saw_wait = true;
                if let Some(resume) = resume.take() {
                    resume.send(()).unwrap();
                }
            }
        })
        .unwrap();
    assert!(saw_wait);
    assert!(!pipeline.snapshot().tape_waiting);
    assert_eq!(pipeline.producer.borrow().allocated, 1);
    pipeline.finish().unwrap();
    let mut paths = Vec::new();
    fn files(dir: &Path, paths: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                files(&p, paths);
            } else {
                paths.push(p);
            }
        }
    }
    files(&root, &mut paths);
    assert_eq!(paths.len(), 1);
    assert_eq!(std::fs::read(&paths[0]).unwrap(), vec![3; READ_BATCH_BYTES]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn descriptor_bound_prevents_unlimited_control_messages() {
    let (pipeline, resume, root) = paused(READ_BATCH_BYTES, "metadata");
    let cancel = pipeline.cancellation.clone();
    for _ in 0..READ_BATCH_ENTRIES {
        pipeline.send(ReadPipelineMessage::ExtentEnd).unwrap();
    }
    let result = pipeline.send_observed(ReadPipelineMessage::ExtentEnd, &mut |sample| {
        if sample.tape_waiting {
            cancel.request_cancel();
        }
    });
    assert!(result.unwrap_err().contains("cancelled"));
    assert_eq!(pipeline.producer.borrow().allocated, 1);
    resume.send(()).unwrap();
    assert!(pipeline.abort().unwrap_err().contains("cancelled"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn slab_boundaries_preserve_file_payload_and_extent_end_order() {
    let root = std::env::temp_dir().join(format!("tapecpy-batch-cross-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let plan = restore_tests::host_plan((READ_BATCH_BYTES + 13) as u64, 2, 524288, false);
    let outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    let pipeline = ReadDestinationPipeline::spawn(
        plan.clone(),
        outputs,
        CancellationToken::default(),
        READ_BATCH_BYTES,
    )
    .unwrap();
    for index in 0..2 {
        pipeline
            .send(ReadPipelineMessage::ExtentStart {
                file_index: index,
                file_offset: 0,
            })
            .unwrap();
        pipeline
            .send_data(vec![index as u8 + 1; READ_BATCH_BYTES - 7])
            .unwrap();
        pipeline.send_data(vec![index as u8 + 11; 20]).unwrap();
        pipeline.send(ReadPipelineMessage::ExtentEnd).unwrap();
    }
    pipeline.finish().unwrap();
    for (index, file) in plan.files.iter().enumerate() {
        let data = std::fs::read(root.join(&file.relative_path)).unwrap();
        assert_eq!(
            &data[..READ_BATCH_BYTES - 7],
            vec![index as u8 + 1; READ_BATCH_BYTES - 7]
        );
        assert_eq!(&data[READ_BATCH_BYTES - 7..], vec![index as u8 + 11; 20]);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn writer_failure_wakes_full_budget_wait_and_preserves_actual_error() {
    let (pipeline, resume, root) = paused(READ_BATCH_BYTES, "disconnect");
    // No ExtentStart: the real worker must fail once it is released.
    pipeline.send_data(vec![1; READ_BATCH_BYTES]).unwrap();
    let mut resume = Some(resume);
    let error = pipeline
        .send_observed(ReadPipelineMessage::ExtentEnd, &mut |sample| {
            if sample.tape_waiting
                && let Some(resume) = resume.take()
            {
                resume.send(()).unwrap();
            }
        })
        .unwrap_err();
    assert!(error.contains("recycle channel"));
    assert!(!pipeline.snapshot().tape_waiting);
    assert!(pipeline.abort().unwrap_err().contains("extent 外"));
    std::fs::remove_dir_all(root).unwrap();
}
