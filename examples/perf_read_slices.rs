//! Hardware fixture: restore three known source slices using nonzero byteoffset plans.
//! This exercises the real reader and persisted plan without altering the LTFS index.
//! It does not replace interoperability testing with an independently authored LTFS volume.
use std::path::{Path, PathBuf};
use tapecpy::{app, device::lease, job};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("_job-runner") && args.len() == 3 {
        return job::run_detached(Path::new(&args[1]), &job::JobId::parse(&args[2])?);
    }
    if args.len() != 3 {
        return Err(
            "usage: perf_read_slices <drive> <single-extent carrier> <NEW destination>".into(),
        );
    }
    let drives = app::discover_drives().map_err(|error| error.to_string())?;
    let drive = app::select_drive(&drives, Some(&args[0]))?;
    let device_lock = lease::DeviceLease::try_acquire(
        &drive.serial,
        lease::LeaseOwner::new("hardware-test", "slice-read-preflight"),
    )?;
    let volume = app::inspect_volume(drive).map_err(|error| error.to_string())?;
    let blocksize = volume.label.ok_or("no label")?.blocksize;
    let index = volume.index.ok_or("no index")?;
    let carrier = index.find_file(&args[1]).ok_or("carrier not found")?;
    if carrier.extents.len() != 1
        || carrier.extents[0].byte_offset != 0
        || carrier.length < 2_000_000
    {
        return Err("fixture requires a zero-offset single-extent carrier of at least 2 MB".into());
    }
    let extent = &carrier.extents[0];
    let mut plan = app::ReadPlan {
        volume_uuid: index.volume_uuid.clone(),
        index_generation: index.generation,
        selections: vec![args[1].clone()],
        directories: Vec::new(),
        files: Vec::new(),
        extents: Vec::new(),
        payload_bytes: 0,
    };
    for (file_index, (offset, count)) in [(17u64, 123u64), (140, 600_000), (600_140, 1_048_700)]
        .into_iter()
        .enumerate()
    {
        let name = format!("slice-{file_index}.bin");
        plan.files.push(app::PlannedReadFile {
            tape_path: format!("{}#{name}", args[1]),
            relative_path: name.into(),
            length: count,
        });
        plan.extents.push(app::PlannedReadExtent {
            file_index,
            file_offset: 0,
            partition: extent.partition,
            start_block: extent.start_block + offset / blocksize,
            byte_offset: offset % blocksize,
            byte_count: count,
        });
        plan.payload_bytes += count;
    }
    let destination = PathBuf::from(&args[2]);
    std::fs::create_dir(&destination).map_err(|error| error.to_string())?;
    app::validate_read_plan_destination(&plan, &destination)?;
    let mount = app::mounted_filesystem_for_path(&destination)?;
    let spec = job::JobSpec::new(
        job::OperationKind::Read,
        drive.sg_path.display().to_string(),
        drive.serial.clone(),
        job::Endpoint {
            path: args[1].clone(),
            filesystem_type: None,
            mount_source: None,
        },
        job::Endpoint {
            path: destination.display().to_string(),
            filesystem_type: mount.as_ref().map(|mount| mount.filesystem_type.clone()),
            mount_source: mount.map(|mount| mount.source),
        },
        false,
    )
    .with_read_preflight(&plan)?;
    drop(device_lock);
    let state = job::spawn_detached(spec, &job::default_job_root()?)?;
    println!(
        "job={} files=3 payload_bytes={}",
        state.spec.id, plan.payload_bytes
    );
    Ok(())
}
