//! tapecpy CLI（Presentation 层）。
//!
//! Milestone 0-5 提供九个命令：
//! - `tapecpy list`：发现并列出磁带机；
//! - `tapecpy info [选择器]`：显示一台磁带机的身份信息；
//! - `tapecpy media [选择器]`：检查介质装载状态并显示基本信息；
//! - `tapecpy volume [选择器]`：识别 LTFS 并显示 volume 基本信息；
//! - `tapecpy ls [选择器] [路径]`：浏览 LTFS 卷目录树；
//! - `tapecpy load [选择器]` / `tapecpy unload [选择器]`：装载/弹出磁带。
//! - `tapecpy read [选择器] <路径> [-o 输出]`：读取 LTFS 卷中的文件。
//! - `tapecpy write <本地路径> <磁带路径> [选择器]`：写入文件或目录树并更新 index。

use std::env;
use std::process::ExitCode;

use tapecpy::app;

const USAGE: &str = "\
tapecpy — Linux 磁带工具（Milestone 3: 设备发现 + 介质检查 + LTFS 识别 + 浏览）

用法:
  tapecpy list                发现并列出系统上的磁带机
  tapecpy info [选择器]        显示一台磁带机的身份信息
  tapecpy media [选择器]       检查介质装载状态并显示基本信息
  tapecpy volume [选择器]      识别 LTFS 并显示 volume 基本信息
  tapecpy ls [选择器] [路径]   浏览 LTFS 卷目录树（默认根目录 /）
  tapecpy load [选择器]        装载磁带（推入后驱动器未识别时使用）
  tapecpy unload [选择器]      弹出磁带
  tapecpy read [选择器] <路径> 读取 LTFS 文件内容到 stdout；-o 指定输出文件
  tapecpy write <本地> <磁带路径>  写入本地文件或目录树（单次更新 index）
                              选择器: 列表序号、/dev/nstX、/dev/stX 或 /dev/sgX
  tapecpy --help              显示本帮助
";

fn main() -> ExitCode {
    // 管道关闭时（如 `tapecpy ls | head`）保持 Unix 惯例：安静退出而非 panic。
    // SAFETY: 恢复 SIGPIPE 默认行为是进程级配置，无数据竞争风险。
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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
        Some("volume") => cmd_volume(args.get(1).map(String::as_str)),
        Some("ls") => cmd_ls(args.get(1).map(String::as_str), args.get(2).map(String::as_str)),
        Some("load") => cmd_load_unload(args.get(1).map(String::as_str), true),
        Some("unload") => cmd_load_unload(args.get(1).map(String::as_str), false),
        Some("read") => cmd_read(&args[1..]),
        Some("write") => cmd_write(&args[1..]),
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

fn cmd_volume(selector: Option<&str>) -> Result<(), String> {
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    let volume = app::inspect_volume(drive).map_err(|e| e.to_string())?;

    print_drive_header(drive);
    println!();

    if !volume.recognized {
        println!("LTFS: 否");
        if let Some(reason) = &volume.reason {
            println!("  原因: {reason}");
        }
        print_warnings(&volume.warnings);
        return Ok(());
    }

    println!("LTFS: 是");
    if let Some(name) = volume
        .index
        .as_ref()
        .and_then(|idx| idx.volume_name())
        .filter(|n| !n.is_empty())
    {
        println!("  Volume Name: {name}");
    }
    if let Some(label) = &volume.label {
        println!("  格式版本:    {}", label.version);
        println!("  Volume UUID: {}", label.volume_uuid);
        println!("  创建程序:    {}", label.creator);
        println!("  格式化时间:  {}", label.format_time);
        println!("  块大小:      {} 字节", label.blocksize);
        println!("  压缩:        {}", yes_no(label.compression));
        println!(
            "  分区:        index={}, data={}",
            label.index_partition, label.data_partition
        );
    }
    if let Some(barcode) = &volume.ansi_barcode {
        println!("  Barcode:     {barcode}（ANSI label）");
    }

    if let Some(idx) = &volume.index {
        println!("  最新 index:  gen {} @ (分区 {}, 块 {})", idx.generation, idx.self_location.partition, idx.self_location.startblock);
        println!("  更新时间:    {}", idx.update_time);
        println!(
            "  文件/目录:   {} / {}",
            idx.file_count(),
            idx.directory_count()
        );
        if let Some(uid) = idx.highest_file_uid {
            println!("  HighestUID:  {uid}");
        }
        if let Some(state) = &idx.volume_lock_state {
            println!("  锁状态:      {state}");
        }
    }
    print_warnings(&volume.warnings);
    Ok(())
}

fn cmd_ls(selector: Option<&str>, path: Option<&str>) -> Result<(), String> {
    use tapecpy::ltfs::index::DirectoryEntry;

    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    let volume = app::inspect_volume(drive).map_err(|e| e.to_string())?;

    if !volume.recognized {
        println!("LTFS: 否（无法浏览）");
        if let Some(reason) = &volume.reason {
            println!("  原因: {reason}");
        }
        return Ok(());
    }
    let Some(index) = &volume.index else {
        println!("没有可用的 index，无法浏览。");
        return Ok(());
    };

    let dir_path = path.unwrap_or("/");
    let dir = index
        .find_directory(dir_path)
        .ok_or_else(|| format!("目录不存在: {dir_path}"))?;

    println!("{}  (gen {})", display_dir_name(dir, dir_path), index.generation);
    for entry in &dir.entries {
        match entry {
            DirectoryEntry::Directory(d) => {
                println!("  d   {:>10}  {}/", "", d.name);
            }
            DirectoryEntry::File(f) => {
                if let Some(target) = &f.symlink_target {
                    println!("  l   {:>10}  {} -> {}", "-", f.name, target);
                } else {
                    println!("  -   {:>10}  {}", format_size(f.length), f.name);
                }
            }
        }
    }
    Ok(())
}

fn cmd_load_unload(selector: Option<&str>, load: bool) -> Result<(), String> {
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    if load {
        app::load_tape(drive).map_err(|e| e.to_string())?;
        println!("已请求装载 {}", drive.nst_path.display());
    } else {
        app::unload_tape(drive).map_err(|e| e.to_string())?;
        println!("已请求弹出 {}", drive.nst_path.display());
    }
    Ok(())
}

fn cmd_read(args: &[String]) -> Result<(), String> {
    use std::io::Write;

    let (selector, rest) = if args.len() >= 2 && looks_like_selector(&args[0]) {
        (Some(args[0].as_str()), &args[1..])
    } else {
        (None, args)
    };
    let path = rest
        .first()
        .ok_or("用法: tapecpy read [选择器] <路径> [-o 输出文件]")?;

    let mut out_path: Option<&str> = None;
    let mut i = 1;
    while i < rest.len() {
        if rest[i] == "-o" || rest[i] == "--output" {
            out_path = rest.get(i + 1).map(String::as_str);
            i += 2;
        } else {
            i += 1;
        }
    }

    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;

    match out_path {
        Some(out) => {
            let mut file = std::fs::File::create(out)
                .map_err(|e| format!("创建 {} 失败: {e}", out))?;
            let n = app::read_file(drive, path, &mut file)?;
            eprintln!("已读取 {n} 字节 -> {out}");
        }
        None => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let n = app::read_file(drive, path, &mut lock)?;
            let _ = lock.flush();
            eprintln!("已读取 {n} 字节 -> stdout");
        }
    }
    Ok(())
}

fn cmd_write(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("用法: tapecpy write <本地路径> <磁带路径> [选择器]".into());
    }
    let local = &args[0];
    let tape_path = &args[1];
    let selector = args.get(2).map(String::as_str);

    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    let mut last_completed = 0usize;
    let mut observer = |event: &app::WriteEvent| match event.phase {
        app::WritePhase::Preparing => eprintln!(
            "准备写入 {} 个文件 / {} 字节",
            event.files_total, event.bytes_total
        ),
        app::WritePhase::WritingData => {
            if event.files_completed > last_completed {
                eprintln!(
                    "  已完成 {}/{}（{}/{} 字节）",
                    event.files_completed,
                    event.files_total,
                    event.bytes_written,
                    event.bytes_total
                );
                last_completed = event.files_completed;
            } else if let Some(path) = &event.current_file {
                eprintln!(
                    "写入文件 {}/{}: {}",
                    event.files_completed + 1,
                    event.files_total,
                    path
                );
            }
        }
        app::WritePhase::FinalizingDataIndex => {
            eprintln!("正在完成 data 分区 index……")
        }
        app::WritePhase::SyncingIndexPartition => {
            eprintln!("正在同步 index 分区……")
        }
        app::WritePhase::Completed => eprintln!("写入会话已安全完成。"),
    };
    let result = app::WriteSession::new(drive).run(
        std::path::Path::new(local),
        tape_path,
        &mut observer,
    )?;
    println!(
        "已写入 {} 个文件 / {} 个目录 / {} 字节 -> {} (highest uid {}, index gen {})",
        result.files,
        result.directories,
        result.bytes,
        tape_path,
        result.file_uid,
        result.generation
    );
    Ok(())
}

fn looks_like_selector(s: &str) -> bool {
    s.starts_with("/dev/") || s.parse::<usize>().is_ok()
}

fn display_dir_name(dir: &tapecpy::ltfs::index::Directory, path: &str) -> String {
    if path == "/" || path.is_empty() {
        format!("{}/", dir.name)
    } else {
        format!("{path}/")
    }
}

fn print_warnings(warnings: &[String]) {
    for w in warnings {
        println!("  警告: {w}");
    }
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

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}
