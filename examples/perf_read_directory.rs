//! Headless hardware-test entry point for the existing TUI directory-read workflow.
//! Uses the same lease, frozen plan, detached runner and create-new destination semantics.
use std::path::{Path, PathBuf};
use tapecpy::{app, device::lease, job};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("_job-runner") && args.len() == 3 {
        return job::run_detached(Path::new(&args[1]), &job::JobId::parse(&args[2])?);
    }
    if args.len() != 3 {
        return Err(
            "usage: perf_read_directory <drive> <LTFS selection> <NEW destination directory>"
                .into(),
        );
    }
    let drives = app::discover_drives().map_err(|error| error.to_string())?;
    let drive = app::select_drive(&drives, Some(&args[0]))?;
    let device_lock = lease::DeviceLease::try_acquire(
        &drive.serial,
        lease::LeaseOwner::new("hardware-test", "directory-read-preflight"),
    )?;
    let volume = app::inspect_volume(drive).map_err(|error| error.to_string())?;
    let index = volume.index.ok_or("no usable LTFS index")?;
    let plan = app::plan_ltfs_read(&index, &[args[1].clone()])?;
    let destination = PathBuf::from(&args[2]);
    std::fs::create_dir(&destination)
        .map_err(|error| format!("create NEW destination: {error}"))?;
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
        "job={} files={} payload_bytes={}",
        state.spec.id,
        plan.files.len(),
        plan.payload_bytes
    );
    Ok(())
}
