//! Read-only inventory of LTFS hashes. Writes reports only to a new host directory.
use std::{collections::BTreeMap, path::PathBuf};
use tapecpy::{
    app,
    device::lease,
    ltfs::index::{Directory, DirectoryEntry},
};
fn walk(
    dir: &Directory,
    prefix: &str,
    files: &mut Vec<serde_json::Value>,
    counts: &mut BTreeMap<String, usize>,
    bytes: &mut u64,
) {
    for entry in &dir.entries {
        let path = format!("{prefix}/{}", entry.name());
        match entry {
            DirectoryEntry::Directory(child) => walk(child, &path, files, counts, bytes),
            DirectoryEntry::File(file) => {
                let hashes: BTreeMap<_, _> = file
                    .extended_attributes
                    .iter()
                    .filter(|a| a.key.starts_with("ltfs.hash."))
                    .map(|a| (a.key.clone(), a.value.clone()))
                    .collect();
                for key in hashes.keys() {
                    *counts.entry(key.clone()).or_default() += 1;
                }
                *bytes += file.length;
                files.push(serde_json::json!({"path":path,"length":file.length,"hashes":hashes,"extents":file.extents.len(),"symlink":file.symlink_target}));
            }
        }
    }
}
fn main() -> Result<(), String> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("NEW output directory required")?,
    );
    std::fs::create_dir(&out).map_err(|e| e.to_string())?;
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, Some("/dev/sg0"))?;
    let _lease = lease::DeviceLease::try_acquire(
        &drive.serial,
        lease::LeaseOwner::new("audit", "inventory-only"),
    )?;
    let volume = app::inspect_volume(drive).map_err(|e| e.to_string())?;
    let index = volume.index.ok_or("No readable index")?;
    std::fs::write(out.join("parsed-index.xml"), index.to_xml()).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    let mut counts = BTreeMap::new();
    let mut bytes = 0;
    walk(&index.root, "", &mut files, &mut counts, &mut bytes);
    let report = serde_json::json!({"uuid":index.volume_uuid,"generation":index.generation,"creator":index.creator,"volume_name":index.root.name,"files":files.len(),"bytes":bytes,"hash_counts":counts,"warnings":volume.warnings});
    std::fs::write(out.join("files.json"), serde_json::to_vec(&files).unwrap())
        .map_err(|e| e.to_string())?;
    std::fs::write(
        out.join("inventory.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}
