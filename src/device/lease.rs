//! 跨 TUI、CLI 和 detached runner 的磁带机进程间所有权租约。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseOwner {
    pub kind: &'static str,
    pub operation: String,
    pub job_id: Option<String>,
}

impl LeaseOwner {
    pub fn new(kind: &'static str, operation: impl Into<String>) -> Self {
        Self {
            kind,
            operation: operation.into(),
            job_id: None,
        }
    }

    pub fn job_runner(operation: impl Into<String>, job_id: impl Into<String>) -> Self {
        Self {
            kind: "job-runner",
            operation: operation.into(),
            job_id: Some(job_id.into()),
        }
    }
}

pub struct DeviceLease {
    _file: File,
}

impl DeviceLease {
    pub fn try_acquire(serial: &str, owner: LeaseOwner) -> Result<Self, String> {
        Self::try_acquire_in(&runtime_lock_root()?, serial, owner)
    }

    pub(crate) fn try_acquire_in(
        root: &Path,
        serial: &str,
        owner: LeaseOwner,
    ) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|error| format!("创建 device lease 目录失败: {error}"))?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("设置 device lease 目录权限失败: {error}"))?;
        let path = root.join(format!("{}.lock", safe_serial(serial)));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| format!("打开 device lease 失败: {error}"))?;
        // SAFETY: File 在 DeviceLease 生命周期内保持有效，drop 时内核自动释放 flock。
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let mut metadata = String::new();
            let _ = file.seek(SeekFrom::Start(0));
            let _ = file.read_to_string(&mut metadata);
            let owner = metadata.trim();
            return Err(if owner.is_empty() {
                format!("磁带机 {serial} 已被另一进程占用")
            } else {
                format!("磁带机 {serial} 已被占用：{owner}")
            });
        }
        file.set_len(0)
            .map_err(|error| format!("清空 device lease metadata 失败: {error}"))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("定位 device lease metadata 失败: {error}"))?;
        let job = owner
            .job_id
            .as_deref()
            .map_or_else(String::new, |id| format!(" job={id}"));
        write!(
            file,
            "pid={} kind={} operation={}{}",
            std::process::id(),
            owner.kind,
            owner.operation,
            job
        )
        .map_err(|error| format!("写入 device lease metadata 失败: {error}"))?;
        file.sync_data()
            .map_err(|error| format!("同步 device lease metadata 失败: {error}"))?;
        Ok(Self { _file: file })
    }
}

fn runtime_lock_root() -> Result<PathBuf, String> {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(runtime).join("tapecpy/device-locks"));
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state).join("tapecpy/device-locks"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "无法确定 XDG_RUNTIME_DIR、XDG_STATE_HOME 或 HOME".to_string())?;
    Ok(PathBuf::from(home).join(".local/state/tapecpy/device-locks"))
}

fn safe_serial(serial: &str) -> String {
    serial
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_is_exclusive_and_reports_owner_metadata() {
        let root = std::env::temp_dir().join(format!(
            "tapecpy-lease-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("lease")
        ));
        let first = DeviceLease::try_acquire_in(
            &root,
            "SERIAL/ONE",
            LeaseOwner::job_runner("Write", "job-1"),
        )
        .unwrap();
        let error =
            DeviceLease::try_acquire_in(&root, "SERIAL/ONE", LeaseOwner::new("cli", "diagnose"))
                .err()
                .unwrap();
        assert!(error.contains("kind=job-runner"));
        assert!(error.contains("job=job-1"));
        drop(first);
        DeviceLease::try_acquire_in(&root, "SERIAL/ONE", LeaseOwner::new("cli", "diagnose"))
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
