//! Detached read-only audit runner; persisted JSON evidence, no payload output.
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tapecpy::app;
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
fn save(out: &std::path::Path, state: &serde_json::Value) -> Result<(), String> {
    let tmp = out.join("progress.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("progress write: {e}"))?;
    std::fs::rename(tmp, out.join("progress.json")).map_err(|e| format!("progress rename: {e}"))
}
fn outcome(
    persistence_failed: bool,
    cancelled: bool,
    result: &Result<(), String>,
    checked: u64,
    total: Option<u64>,
    failed: u64,
) -> &'static str {
    if persistence_failed {
        "error"
    } else if cancelled {
        "interrupted"
    } else if result.is_ok() {
        "completed"
    } else if failed > 0 && Some(checked) == total {
        "completed_with_mismatches"
    } else {
        "read_failed"
    }
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "usage: perf_audit_sha1 <inventory-dir> <previous-files.jsonl> <NEW-output-dir>".into(),
        );
    }
    let inventory: serde_json::Value = serde_json::from_slice(
        &std::fs::read(PathBuf::from(&args[0]).join("inventory.json"))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let old = PathBuf::from(&args[1]);
    let mut previous = BTreeMap::new();
    for line in std::fs::read_to_string(old)
        .map_err(|e| e.to_string())?
        .lines()
    {
        let record: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        if record["verified"] == true && record["read_exit_code"] == 0 {
            let hash = record["expected_sha1"].as_str().ok_or("missing old hash")?;
            if !record["actual_sha1"]
                .as_str()
                .is_some_and(|h| h.eq_ignore_ascii_case(hash))
                || record["bytes"] != record["expected_bytes"]
            {
                return Err("invalid previous success record".into());
            }
            previous.insert(
                record["path"].as_str().ok_or("old path")?.into(),
                (record["bytes"].as_u64().ok_or("old bytes")?, hash.into()),
            );
        }
    }
    let out = PathBuf::from(&args[2]);
    std::fs::create_dir(&out).map_err(|e| e.to_string())?;
    let mut log = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("events.jsonl"))
        .map_err(|e| e.to_string())?;
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, Some("/dev/sg0"))?;
    let token = app::CancellationToken::default();
    // SAFETY: handler only sets an atomic flag. The monitor forwards cancellation.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let mut state = serde_json::json!({"status":"running","volume":inventory,"files_checked":0,"passed":0,"failed":0,"resumed_previous":0,"bytes_checked":0,"current_bytes":0,"current_file":null});
    save(&out, &state)?;
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let done_worker = done.clone();
    let monitor_token = token.clone();
    let monitor = std::thread::spawn(move || {
        while !done_worker.load(Ordering::Relaxed) {
            if STOP.load(Ordering::Relaxed) {
                monitor_token.request_cancel();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let mut observation_error = None;
    let result = app::audit_sha1_volume(drive, &previous, &token, &mut |event| {
        if observation_error.is_some() {
            return;
        }
        let observed = (|| -> Result<(), String> {
            use std::io::Write;
            state["files_total"] = event.files_total.into();
            state["current_file"] = event.path.clone().into();
            state["current_bytes"] = event.bytes_read.into();
            state["current_seconds"] = event.seconds.into();
            state["message"] = event.status.into();
            let terminal = matches!(
                event.status,
                "passed" | "mismatch" | "skipped_previous_pass" | "read_failed"
            );
            if terminal {
                writeln!(
                    log,
                    "{}",
                    serde_json::to_string(event).map_err(|e| e.to_string())?
                )
                .map_err(|e| format!("result write: {e}"))?;
                log.sync_all().map_err(|e| format!("result sync: {e}"))?;
                if event.status != "read_failed" {
                    state["files_checked"] = (state["files_checked"].as_u64().unwrap() + 1).into();
                    state["bytes_checked"] =
                        (state["bytes_checked"].as_u64().unwrap() + event.length).into();
                    let key = if event.status == "mismatch" {
                        "failed"
                    } else {
                        "passed"
                    };
                    state[key] = (state[key].as_u64().unwrap() + 1).into();
                    if event.status == "skipped_previous_pass" {
                        state["resumed_previous"] =
                            (state["resumed_previous"].as_u64().unwrap() + 1).into();
                    }
                }
                println!(
                    "{}",
                    serde_json::to_string(event).map_err(|e| e.to_string())?
                );
            }
            save(&out, &state)
        })();
        if let Err(error) = observed {
            observation_error = Some(error);
            token.request_cancel();
        }
    });
    done.store(true, Ordering::Relaxed);
    monitor.join().map_err(|_| "monitor panic")?;
    let result = if let Some(error) = &observation_error {
        Err(error.clone())
    } else {
        result
    };
    state["status"] = outcome(
        observation_error.is_some(),
        token.is_cancelled(),
        &result,
        state["files_checked"].as_u64().unwrap(),
        state["files_total"].as_u64(),
        state["failed"].as_u64().unwrap(),
    )
    .into();
    state["error"] = result.as_ref().err().cloned().into();
    save(&out, &state)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_status_distinguishes_io_cancel_and_persistence_failures() {
        let error = Err("failure".into());
        assert_eq!(outcome(false, false, &error, 2, Some(3), 1), "read_failed");
        assert_eq!(
            outcome(false, false, &error, 3, Some(3), 1),
            "completed_with_mismatches"
        );
        assert_eq!(outcome(false, false, &error, 0, None, 0), "read_failed");
        assert_eq!(outcome(false, true, &error, 2, Some(3), 0), "interrupted");
        assert_eq!(outcome(true, true, &error, 2, Some(3), 0), "error");
        assert_eq!(outcome(false, false, &Ok(()), 3, Some(3), 0), "completed");
    }
    #[test]
    fn progress_io_error_returns_error_instead_of_panicking() {
        assert!(
            save(
                std::path::Path::new("/dev/null"),
                &serde_json::json!({"status":"running"})
            )
            .is_err()
        );
    }
}
