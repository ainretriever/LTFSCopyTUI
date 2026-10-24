use super::*;
fn setup(name: &str, length: u64, extents: usize) -> (PathBuf, ReadPlan) {
    let root = std::env::temp_dir().join(format!("tapecpy-sync-{name}-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let plan = ReadPlan {
        volume_uuid: "test".into(),
        index_generation: 0,
        selections: vec![],
        directories: vec!["a/b".into()],
        files: vec![PlannedReadFile {
            tape_path: "/a/b/file".into(),
            relative_path: "a/b/file".into(),
            length,
        }],
        extents: (0..extents)
            .map(|i| PlannedReadExtent {
                file_index: 0,
                file_offset: i as u64 * 2,
                partition: 1,
                start_block: i as u64,
                byte_offset: 0,
                byte_count: 2,
            })
            .collect(),
        payload_bytes: length,
    };
    (root, plan)
}
#[test]
fn final_extent_sync_failure_does_not_publish_completion_and_preserves_error() {
    let (root, plan) = setup("failure", 2, 1);
    let mut outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    // /dev/null accepts writes but rejects fsync: exercise a real OS sync failure.
    outputs.handles[0] = Some(
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .unwrap(),
    );
    let pipeline =
        ReadDestinationPipeline::spawn(plan, outputs, CancellationToken::default(), 1024 * 1024)
            .unwrap();
    pipeline
        .send(ReadPipelineMessage::ExtentStart {
            file_index: 0,
            file_offset: 0,
        })
        .unwrap();
    pipeline.send_data(vec![1, 2]).unwrap();
    pipeline.send(ReadPipelineMessage::ExtentEnd).unwrap();
    pipeline.flush_batch().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !pipeline.worker.as_ref().unwrap().is_finished() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(pipeline.files_completed(), 0);
    let error = pipeline.finish().unwrap_err();
    assert!(error.contains("同步恢复文件"), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn multi_extent_syncs_only_after_last_extent_and_nested_namespace_finishes() {
    let (root, plan) = setup("multi", 4, 2);
    let mut outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    let file = outputs.open_extent(&plan, 0).unwrap();
    file.write_at(b"ab", 0).unwrap();
    assert!(!outputs.finish_extent(&plan, 0, file).unwrap());
    let file = outputs.open_extent(&plan, 0).unwrap();
    file.write_at(b"cd", 2).unwrap();
    assert!(outputs.finish_extent(&plan, 0, file).unwrap());
    assert_eq!(
        outputs.directories_to_sync,
        vec![PathBuf::from("a/b"), PathBuf::from("a")]
    );
    outputs.sync_filesystem().unwrap();
    assert_eq!(std::fs::read(root.join("a/b/file")).unwrap(), b"abcd");
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn empty_and_sparse_outputs_are_synced_and_sync_errors_are_not_ignored() {
    let (root, plan) = setup("empty", 0, 0);
    let outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    outputs.sync_filesystem().unwrap();
    assert_eq!(std::fs::metadata(root.join("a/b/file")).unwrap().len(), 0);
    let null = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();
    assert!(
        sync_read_output(&null, Path::new("injected-empty-output"))
            .unwrap_err()
            .contains("injected-empty-output")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn nonfinal_extent_defers_file_sync_but_final_extent_cannot_ignore_failure() {
    let (root, plan) = setup("deferred", 4, 2);
    let mut outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    let null = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();
    assert!(!outputs.finish_extent(&plan, 0, null).unwrap());
    let null = outputs.open_extent(&plan, 0).unwrap();
    assert!(
        outputs
            .finish_extent(&plan, 0, null)
            .unwrap_err()
            .contains("同步恢复文件")
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn namespace_sync_failure_fails_finish_even_after_all_file_data_completed() {
    let (root, plan) = setup("namespace-failure", 0, 0);
    let mut outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    outputs.directories_to_sync.clear();
    outputs.sync_anchor = std::fs::File::open("/dev/null").unwrap();
    let pipeline =
        ReadDestinationPipeline::spawn(plan, outputs, CancellationToken::default(), 1048576)
            .unwrap();
    assert_eq!(pipeline.files_completed(), 1);
    assert!(pipeline.finish().unwrap_err().contains("同步恢复根目录"));
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn sparse_file_with_no_data_extents_is_sized_and_synced_before_completion() {
    let (root, plan) = setup("sparse", 4096, 0);
    let outputs = ReadOutputFiles::prepare(&plan, &root).unwrap();
    outputs.sync_filesystem().unwrap();
    assert_eq!(std::fs::read(root.join("a/b/file")).unwrap(), vec![0; 4096]);
    std::fs::remove_dir_all(root).unwrap();
}
