//! sysfs 设备枚举与 nst/sg 匹配。
//!
//! 匹配原理：/sys/class/scsi_tape/nst0 与 /sys/class/scsi_generic/sg1
//! 都是指向同一个 SCSI 设备目录的符号链接，解析后比较 SCSI 设备目录即可。

use std::fs;
use std::path::{Component, Path, PathBuf};

/// 枚举 /sys/class/scsi_tape 下的基本无回绕设备名（如 "nst0"）。
///
/// 排除带后缀的变体（nst0a/nst0l/nst0m）以及回绕设备 st0：
/// 它们与基本节点指向同一个磁带机，只需要一个入口。
pub fn enumerate_tape_bases(sysfs_root: &Path) -> Result<Vec<String>, std::io::Error> {
    let class_dir = sysfs_root.join("class/scsi_tape");
    if !class_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(&class_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_base_nst(&name) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn is_base_nst(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("nst") else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

/// 为指定 nst 设备查找对应的 SCSI generic 设备名（如 "sg1"）。
pub fn find_sg_for_tape(sysfs_root: &Path, nst_name: &str) -> Option<String> {
    let tape_dev = scsi_device_dir(sysfs_root, "scsi_tape", nst_name)?;
    let sg_class_dir = sysfs_root.join("class/scsi_generic");
    let entries = fs::read_dir(&sg_class_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("sg") || name[2..].bytes().any(|b| !b.is_ascii_digit()) {
            continue;
        }
        if scsi_device_dir(sysfs_root, "scsi_generic", &name) == Some(tape_dev.clone()) {
            return Some(name);
        }
    }
    None
}

/// 解析某个 class 条目（如 scsi_tape/nst0）指向的 SCSI 设备目录。
///
/// 例如 `/sys/devices/.../6:0:0:0/scsi_tape/nst0` 解析为
/// `/sys/devices/.../6:0:0:0`。
fn scsi_device_dir(sysfs_root: &Path, class: &str, name: &str) -> Option<PathBuf> {
    let resolved = fs::canonicalize(sysfs_root.join(format!("class/{class}/{name}"))).ok()?;
    scsi_device_dir_from_path(&resolved)
}

fn scsi_device_dir_from_path(path: &Path) -> Option<PathBuf> {
    let components: Vec<Component> = path.components().collect();
    let pos = components.iter().position(|c| match c {
        Component::Normal(s) => is_scsi_device_id(&s.to_string_lossy()),
        _ => false,
    })?;
    let mut out = PathBuf::new();
    for c in &components[..=pos] {
        out.push(c.as_os_str());
    }
    Some(out)
}

fn is_scsi_device_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn temp_sysfs() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tapecpy-sysfs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_class_entry(root: &Path, class: &str, name: &str, target: &Path) {
        let class_dir = root.join("class").join(class);
        fs::create_dir_all(&class_dir).unwrap();
        // 真实 sysfs 中符号链接目标一定存在；测试里也补上目标节点。
        if !target.exists() {
            fs::write(target, b"").unwrap();
        }
        symlink(target, class_dir.join(name)).unwrap();
    }

    #[test]
    fn scsi_device_dir_extraction() {
        let p = Path::new(
            "/sys/devices/pci0000:00/0000:00:01.0/host6/target6:0:0/6:0:0:0/scsi_tape/nst0",
        );
        assert_eq!(
            scsi_device_dir_from_path(p).unwrap(),
            PathBuf::from("/sys/devices/pci0000:00/0000:00:01.0/host6/target6:0:0/6:0:0:0")
        );
        assert_eq!(
            scsi_device_dir_from_path(Path::new("/sys/class/scsi_tape/nst0")),
            None
        );
    }

    #[test]
    fn enumerate_and_match_fake_sysfs() {
        let root = temp_sysfs();
        let dev = root.join("devices/test/6:0:0:0");
        let tape_dir = dev.join("scsi_tape");
        let sg_dir = dev.join("scsi_generic");
        fs::create_dir_all(&tape_dir).unwrap();
        fs::create_dir_all(&sg_dir).unwrap();

        // 基本节点与所有变体都指向同一个磁带目录
        for name in ["nst0", "nst0a", "nst0l", "nst0m", "st0"] {
            make_class_entry(&root, "scsi_tape", name, &tape_dir.join(name));
        }
        make_class_entry(&root, "scsi_generic", "sg1", &sg_dir.join("sg1"));

        let bases = enumerate_tape_bases(&root).unwrap();
        assert_eq!(bases, vec!["nst0".to_string()]);
        assert_eq!(find_sg_for_tape(&root, "nst0").as_deref(), Some("sg1"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unmatched_sg_is_ignored() {
        let root = temp_sysfs();
        let dev = root.join("devices/test/6:0:0:0");
        fs::create_dir_all(dev.join("scsi_tape")).unwrap();
        // 只有一个不相关的 sg（例如 SATA 磁盘），不应被匹配上
        let disk = root.join("devices/ata/0:0:0:0");
        fs::create_dir_all(disk.join("scsi_generic")).unwrap();
        make_class_entry(&root, "scsi_tape", "nst0", &dev.join("scsi_tape/nst0"));
        make_class_entry(&root, "scsi_generic", "sg0", &disk.join("scsi_generic/sg0"));

        assert_eq!(
            enumerate_tape_bases(&root).unwrap(),
            vec!["nst0".to_string()]
        );
        assert_eq!(find_sg_for_tape(&root, "nst0"), None);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn base_nst_name_rule() {
        assert!(is_base_nst("nst0"));
        assert!(is_base_nst("nst12"));
        assert!(!is_base_nst("nst0a"));
        assert!(!is_base_nst("st0"));
        assert!(!is_base_nst("nst"));
    }
}
