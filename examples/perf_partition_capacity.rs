//! Read-only evidence for partition-specific capacity preflight diagnostics.
use tapecpy::{app, device};

fn main() -> Result<(), String> {
    let selector = std::env::args().nth(1).unwrap_or("/dev/sg0".into());
    let drives = app::discover_drives().map_err(|error| error.to_string())?;
    let drive = app::select_drive(&drives, Some(&selector))?;
    let _lease = device::lease::DeviceLease::try_acquire(
        &drive.serial,
        device::lease::LeaseOwner::new("hardware-test", "partition-capacity"),
    )?;
    let volume = app::inspect_volume(drive).map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::json!({"preflight_data_remaining_MiB": volume.data_remaining_capacity_mib,
        "payload_100GiB_status":format!("{:?}", app::assess_write_capacity(100 * 1024_u64.pow(3), volume.data_remaining_capacity_mib, tapecpy::job::timestamp_now()).status)})
    );
    let label = volume.label.ok_or("No LTFS label")?;
    let mut tape =
        device::tape::TapeSession::open(&drive.sg_path).map_err(|error| error.to_string())?;
    for partition in [label.index_partition, label.data_partition] {
        let raw = tape
            .read_mam_attribute(partition, 0x0000)
            .map_err(|error| error.to_string())?;
        let remaining = device::scsi::u64_value(&raw).ok_or("Invalid capacity attribute")?;
        println!(
            "{}",
            serde_json::json!({"partition": partition,
            "role": if partition == label.data_partition {"data"} else {"index"},
            "remaining_MiB": remaining})
        );
    }
    Ok(())
}
