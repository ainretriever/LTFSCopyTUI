//! Query temperature only after acquiring the same exclusive device lease as jobs.
use std::time::{Duration, Instant};
use tapecpy::{app, device};

fn main() -> Result<(), String> {
    let drives = app::discover_drives().map_err(|error| error.to_string())?;
    let drive = app::select_drive(&drives, Some("/dev/sg0"))?;
    let deadline = Instant::now() + Duration::from_secs(900);
    let _lease = loop {
        match device::lease::DeviceLease::try_acquire(
            &drive.serial,
            device::lease::LeaseOwner::new("hardware-test", "temperature"),
        ) {
            Ok(lease) => break lease,
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    };
    let mut tape =
        device::tape::TapeSession::open(&drive.sg_path).map_err(|error| error.to_string())?;
    let raw = tape
        .read_log_page(0x0d, 0)
        .map_err(|error| error.to_string())?;
    let parameters = device::log::parse_page(&raw, 0x0d).map_err(|error| format!("{error:?}"))?;
    let value = device::log::temperature_log(&parameters);
    println!(
        "{}",
        serde_json::json!({"current_celsius":value.current_celsius,
        "reference_celsius":value.reference_celsius,
        "sampled_at":tapecpy::job::timestamp_now(),"raw_page":raw})
    );
    Ok(())
}
