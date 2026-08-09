//! tapecpy CLI（Presentation 层）。
//!
//! Milestone 0/1 提供三个命令：
//! - `tapecpy list`：发现并列出磁带机；
//! - `tapecpy info [选择器]`：显示一台磁带机的身份信息；
//! - `tapecpy media [选择器]`：检查介质装载状态并显示基本信息。

use std::env;
use std::process::ExitCode;

use tapecpy::app;

const USAGE: &str = "\
tapecpy — Linux 磁带工具（Milestone 1: 设备发现 + 介质检查）

用法:
  tapecpy list                发现并列出系统上的磁带机
  tapecpy info [选择器]        显示一台磁带机的身份信息
  tapecpy media [选择器]       检查介质装载状态并显示基本信息
                              选择器: 列表序号、/dev/nstX、/dev/stX 或 /dev/sgX
  tapecpy --help              显示本帮助
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("tapecpy: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None | Some("list") | Some("devices") => cmd_list(),
        Some("info") => cmd_info(args.get(1).map(String::as_str)),
        Some("media") => cmd_media(args.get(1).map(String::as_str)),
        Some("--help") | Some("-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("未知命令 `{other}`\n\n{USAGE}")),
    }
}

fn cmd_list() -> Result<(), String> {
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    if drives.is_empty() {
        println!("未发现磁带机。");
        return Ok(());
    }
    println!(
        "{:<6}{:<10}{:<20}{:<16}{:<12}{}",
        "序号", "厂商", "型号", "序列号", "磁带设备", "SCSI 设备"
    );
    for (i, d) in drives.iter().enumerate() {
        println!(
            "{:<6}{:<10}{:<20}{:<16}{:<12}{}",
            i + 1,
            d.vendor,
            d.model,
            d.serial,
            d.nst_path.display(),
            d.sg_path.display()
        );
    }
    Ok(())
}

fn cmd_info(selector: Option<&str>) -> Result<(), String> {
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    print_drive_header(drive);
    Ok(())
}

fn cmd_media(selector: Option<&str>) -> Result<(), String> {
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    let media = app::inspect_media(drive).map_err(|e| e.to_string())?;

    print_drive_header(drive);
    println!();
    println!("介质状态: {}", presence_label(media.presence));

    match media.tape_status {
        Some(st) => {
            println!("  密度:        0x{:02x}", st.density_code);
            if let Some(name) = density_label(st.density_code) {
                println!("  格式:        {name}");
            }
            if st.block_size != 0 {
                println!("  块大小:      {} 字节", st.block_size);
            } else {
                println!("  块大小:      可变");
            }
            println!("  当前分区:    {}", st.partition);
            if st.file_no >= 0 {
                println!("  位置:        文件 {} / 块 {}", st.file_no, st.block_no);
            }
            println!("  写保护:      {}", yes_no(st.is_write_protected()));
            let flags = status_flags(&st);
            if !flags.is_empty() {
                println!("  状态位:      {flags}");
            }
        }
        None => {
            if let Some(code) = media.density_code {
                println!("  密度:        0x{code:02x}");
            }
        }
    }

    if let Some(mam) = &media.mam {
        if mam.barcode.as_deref().is_some_and(|s| !s.is_empty()) {
            println!("  Barcode:     {}", mam.barcode.as_deref().unwrap());
            if let Some(label) = media.full_label_hint() {
                println!(
                    "  标准8位标签: {label}（6 位卷序列 + 介质代际码，供核对物理标签）"
                );
            }
        }
        if let Some(v) = &mam.volume_identifier {
            if !v.is_empty() {
                println!("  MAM 卷标识:  {v}");
            }
        }
        if let Some(v) = &mam.medium_manufacturer {
            if !v.is_empty() {
                println!("  介质厂商:    {v}");
            }
        }
        if let Some(v) = &mam.medium_serial {
            if !v.is_empty() {
                println!("  介质序列号:  {v}");
            }
        }
        if let Some(t) = mam.medium_type {
            println!("  介质类型:    0x{t:02x}");
        }
        if let Some(mib) = mam.remaining_capacity_mib {
            println!("  剩余容量:    {}", format_capacity(mib));
        }
        if let Some(mib) = mam.max_capacity_mib {
            println!("  最大容量:    {}", format_capacity(mib));
        }
        if let Some(n) = mam.load_count {
            println!("  装载次数:    {n}");
        }
        if let Some(flags) = mam.tape_alert_flags {
            if flags != 0 {
                println!("  TapeAlert:   0x{flags:016x}");
            }
        }
        if let Some(mib) = mam.total_written_mib {
            println!("  累计写入:    {}", format_capacity(mib));
        }
        if let Some(mib) = mam.total_read_mib {
            println!("  累计读取:    {}", format_capacity(mib));
        }
    }
    Ok(())
}

fn print_drive_header(drive: &tapecpy::device::TapeDrive) {
    println!(
        "磁带机: {}  (回绕设备 {})",
        drive.nst_path.display(),
        drive.st_path.display()
    );
    println!("  SCSI 设备:   {}", drive.sg_path.display());
    println!("  厂商:        {}", drive.vendor);
    println!("  型号:        {}", drive.model);
    println!("  固件版本:    {}", drive.revision);
    println!("  序列号:      {}", drive.serial);
}

fn presence_label(p: tapecpy::device::MediaPresence) -> &'static str {
    use tapecpy::device::MediaPresence;
    match p {
        MediaPresence::Loaded => "已装载",
        MediaPresence::NotLoaded => "未装载",
        MediaPresence::NotReady => "驱动器未就绪",
        MediaPresence::Unknown => "未知",
    }
}

fn density_label(code: u8) -> Option<&'static str> {
    tapecpy::device::density::density_name(code)
}

fn status_flags(st: &tapecpy::device::mtio::TapeStatus) -> String {
    let mut out = Vec::new();
    if st.is_bot() {
        out.push("BOT");
    }
    if st.is_eof() {
        out.push("EOF");
    }
    if st.is_eot() {
        out.push("EOT");
    }
    if st.is_eod() {
        out.push("EOD");
    }
    if st.is_door_open() {
        out.push("DR_OPEN");
    }
    if st.cleaning_requested() {
        out.push("CLN");
    }
    out.join(" ")
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "是"
    } else {
        "否"
    }
}

fn format_capacity(mib: u64) -> String {
    if mib >= 1024 * 1024 {
        format!("{mib} MiB ({:.2} TiB)", mib as f64 / 1_048_576.0)
    } else if mib >= 1024 {
        format!("{mib} MiB ({:.2} GiB)", mib as f64 / 1024.0)
    } else {
        format!("{mib} MiB")
    }
}
