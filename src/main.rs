//! tapecpy CLI（Presentation 层）。
//!
//! Milestone 0 提供两个命令：
//! - `tapecpy list`：发现并列出磁带机；
//! - `tapecpy info [选择器]`：显示一台磁带机的身份信息。

use std::env;
use std::process::ExitCode;

use tapecpy::app;

const USAGE: &str = "\
tapecpy — Linux 磁带工具（Milestone 0: 设备发现）

用法:
  tapecpy list               发现并列出系统上的磁带机
  tapecpy info [选择器]       显示一台磁带机的身份信息
                             选择器: 列表序号、/dev/nstX、/dev/stX 或 /dev/sgX
  tapecpy --help             显示本帮助
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
    Ok(())
}

