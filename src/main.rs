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
//! - `tapecpy write <本地路径> <磁带路径> [选择器] [--read-back-verify]`：写入并可选回读校验。
//! - `tapecpy format <Barcode> <Volume Name> [选择器] --force`：创建新 LTFS 卷。
//! - `tapecpy erase <short|long|minimum> [选择器] --force`：擦除/准备介质。
//! - `tapecpy mam [选择器]`：显示 LTFS MAM/VCI 诊断信息。
//! - `tapecpy health [选择器]`：显示 LOG SENSE 错误计数和 TapeAlert。
//! - `tapecpy diagnose [选择器]`：只读扫描两分区 index/VCI 一致性。

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
  tapecpy mam [选择器]         显示两个分区的 LTFS MAM/VCI 诊断信息
  tapecpy health [选择器]      显示读写错误累计计数和当前 TapeAlert
  tapecpy diagnose [选择器] [--full]
                              只读诊断；--full 才顺序扫描整个 data partition
  tapecpy ls [选择器] [路径]   浏览 LTFS 卷目录树（默认根目录 /）
  tapecpy load [选择器]        装载磁带（推入后驱动器未识别时使用）
  tapecpy unload [选择器]      弹出磁带
  tapecpy read [选择器] <路径> 读取 LTFS 文件内容到 stdout；-o 指定输出文件
  tapecpy write <本地> <磁带路径> [选择器] [--read-back-verify]
                              写入；该选项在提交后从磁带回读并校验 SHA-256
  tapecpy write-random <大小> <磁带路径> [选择器] [--seed=N]
                              流式写入可重现的伪随机测试数据（如 80GiB）
                              测试可加 --failpoint/--cancelpoint=<语义步骤>（破坏性）
  tapecpy format <Barcode> <Volume Name> [选择器] --force
                              破坏性地重新格式化为 LTFS（必须显式指定 --force）
  tapecpy erase <short|long|minimum> [选择器] --force
                              擦除介质：快速、全带长擦除、最小分区长擦除
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
        Some("mam") => cmd_mam(args.get(1).map(String::as_str)),
        Some("health") => cmd_health(args.get(1).map(String::as_str)),
        Some("diagnose") => cmd_diagnose(&args[1..]),
        Some("ls") => cmd_ls(
            args.get(1).map(String::as_str),
            args.get(2).map(String::as_str),
        ),
        Some("load") => cmd_load_unload(args.get(1).map(String::as_str), true),
        Some("unload") => cmd_load_unload(args.get(1).map(String::as_str), false),
        Some("read") => cmd_read(&args[1..]),
        Some("write") => cmd_write(&args[1..]),
        Some("write-random") => cmd_write_random(&args[1..]),
        Some("format") => cmd_format(&args[1..]),
        Some("erase") => cmd_erase(&args[1..]),
        Some("--help") | Some("-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("未知命令 `{other}`\n\n{USAGE}")),
    }
}

fn cmd_diagnose(args: &[String]) -> Result<(), String> {
    let full = args.iter().any(|argument| argument == "--full");
    let selector = args
        .iter()
        .find(|argument| argument.as_str() != "--full")
        .map(String::as_str);
    let drives = app::discover_drives().map_err(|error| error.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    print_drive_header(drive);
    if full {
        eprintln!("只读完整扫描两个 partition；大卷可能需要数小时。");
    } else {
        eprintln!("只读有界诊断：完整扫描 index partition，定点读取 data index。");
    }
    let diagnosis = if full {
        app::diagnose_volume_full(drive)?
    } else {
        app::diagnose_volume(drive)?
    };
    println!("一致性: {:?}", diagnosis.consistency);
    println!("普通写入安全: {}", diagnosis.safe_for_normal_write);
    if let Some(uuid) = &diagnosis.label_uuid {
        println!("Label UUID: {uuid}");
    }
    for candidate in diagnosis.candidates {
        if let Some(index) = candidate.index {
            println!(
                "P{} B{} {} bytes: index gen={} uuid={} self=p{}b{} previous={:?}",
                candidate.physical_partition,
                candidate.actual_start_block,
                candidate.byte_len,
                index.generation,
                index.volume_uuid,
                index.self_location.partition,
                index.self_location.startblock,
                index.previous_location
            );
        } else {
            println!(
                "P{} B{} {} bytes: INVALID: {}",
                candidate.physical_partition,
                candidate.actual_start_block,
                candidate.byte_len,
                candidate.parse_error.as_deref().unwrap_or("未知解析错误")
            );
        }
    }
    for (partition, vci) in diagnosis.vci {
        println!(
            "P{partition} VCI: gen={} block={} uuid={} vcr={}",
            vci.generation,
            vci.block,
            vci.volume_uuid,
            hex_compact(&vci.vcr)
        );
    }
    print_warnings(&diagnosis.partition_errors);
    Ok(())
}

fn cmd_health(selector: Option<&str>) -> Result<(), String> {
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    let health = app::read_drive_health(drive)?;
    println!("磁带机: {} ({})", drive.sg_path.display(), drive.model);
    print_error_counters("写入", health.write_errors.as_ref());
    print_error_counters("读取", health.read_errors.as_ref());
    if health.tape_alerts.is_empty() {
        println!("TapeAlert: 无活动 flag");
    } else {
        let flags = health
            .tape_alerts
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!("TapeAlert: {flags}");
    }
    for warning in health.warnings {
        eprintln!("警告: {warning}");
    }
    if let Some(channels) = health.write_channels {
        println!("写通道 diagnostic baseline: {} 个通道", channels.len());
    }
    Ok(())
}

fn print_error_counters(label: &str, counters: Option<&tapecpy::device::log::ErrorCounters>) {
    let Some(counters) = counters else {
        println!("{label}错误计数: 不可用");
        return;
    };
    println!(
        "{label}错误计数: corrected={}，uncorrected={}，processed={}",
        display_counter(counters.total_corrected),
        display_counter(counters.uncorrected),
        display_counter(counters.data_processed),
    );
    println!(
        "  without-delay={}，with-delay={}，correction-runs={}",
        display_counter(counters.corrected_without_delay),
        display_counter(counters.corrected_with_delay),
        display_counter(counters.correction_processed),
    );
}

fn display_counter(value: Option<u64>) -> String {
    value.map_or_else(|| "n/a".into(), |value| value.to_string())
}

fn cmd_list() -> Result<(), String> {
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    if drives.is_empty() {
        println!("未发现磁带机。");
        return Ok(());
    }
    println!(
        "{:<6}{:<10}{:<20}{:<16}{:<12}SCSI 设备",
        "序号", "厂商", "型号", "序列号", "磁带设备"
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
                println!("  标准8位标签: {label}（6 位卷序列 + 介质代际码，供核对物理标签）");
            }
        }
        if let Some(v) = &mam.volume_identifier
            && !v.is_empty()
        {
            println!("  MAM 卷标识:  {v}");
        }
        if let Some(v) = &mam.medium_manufacturer
            && !v.is_empty()
        {
            println!("  介质厂商:    {v}");
        }
        if let Some(v) = &mam.medium_serial
            && !v.is_empty()
        {
            println!("  介质序列号:  {v}");
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
        if let Some(flags) = mam.tape_alert_flags
            && flags != 0
        {
            println!("  TapeAlert:   0x{flags:016x}");
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
        println!(
            "  最新 index:  gen {} @ (分区 {}, 块 {})",
            idx.generation, idx.self_location.partition, idx.self_location.startblock
        );
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

fn cmd_mam(selector: Option<&str>) -> Result<(), String> {
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    print_drive_header(drive);
    let report = app::inspect_mam(drive)?;
    for partition in report.partitions {
        println!("\nMAM partition {}:", partition.partition);
        for attribute in partition.attributes {
            print!("  0x{:04X}  format={}  ", attribute.id, attribute.format);
            if let Some(vci) = attribute.vci {
                println!(
                    "VCI vcr={} gen={} block={} uuid={} version={}",
                    hex_compact(&vci.vcr),
                    vci.generation,
                    vci.block,
                    vci.volume_uuid,
                    vci.acsi_version
                );
            } else if matches!(attribute.format, 1 | 2) {
                let text = String::from_utf8_lossy(&attribute.value)
                    .trim_end_matches(['\0', ' '])
                    .to_string();
                println!("{text:?}");
            } else {
                println!("{}", hex_compact(&attribute.value));
            }
            if let Some(warning) = attribute.parse_warning {
                println!("    警告: VCI 解析失败: {warning}");
            }
        }
    }
    print_warnings(&report.warnings);
    Ok(())
}

fn hex_compact(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join("")
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

    println!(
        "{}  (gen {})",
        display_dir_name(dir, dir_path),
        index.generation
    );
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
            let mut file =
                std::fs::File::create(out).map_err(|e| format!("创建 {} 失败: {e}", out))?;
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
    let verify = args.iter().any(|arg| arg == "--read-back-verify");
    let positional: Vec<&str> = args
        .iter()
        .filter(|arg| arg.as_str() != "--read-back-verify")
        .map(String::as_str)
        .collect();
    if positional.len() < 2 || positional.len() > 3 {
        return Err(
            "用法: tapecpy write <本地路径> <磁带路径> [选择器] [--read-back-verify]".into(),
        );
    }
    let local = positional[0];
    let tape_path = positional[1];
    let selector = positional.get(2).copied();

    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, selector)?;
    let mut observer = write_cli_observer();
    let result = app::WriteSession::new(drive).run_with_options(
        std::path::Path::new(local),
        tape_path,
        app::WriteOptions {
            verification: if verify {
                app::WriteVerification::ReadBackSha256
            } else {
                app::WriteVerification::None
            },
            failpoint: None,
            cancellation: None,
            cancelpoint: None,
        },
        &mut observer,
    )?;
    print_write_result(tape_path, &result);
    Ok(())
}

fn cmd_write_random(args: &[String]) -> Result<(), String> {
    let mut positional = Vec::new();
    let mut seed = None;
    let mut failpoint = None;
    let mut cancelpoint = None;
    for arg in args {
        if let Some(value) = arg.strip_prefix("--seed=") {
            seed = Some(
                value
                    .parse::<u64>()
                    .map_err(|_| format!("无效 seed: {value}"))?,
            );
        } else if let Some(value) = arg.strip_prefix("--failpoint=") {
            failpoint = Some(value.parse::<app::WriteFailpoint>()?);
        } else if let Some(value) = arg.strip_prefix("--cancelpoint=") {
            cancelpoint = Some(value.parse::<app::WriteFailpoint>()?);
        } else {
            positional.push(arg.as_str());
        }
    }
    if positional.len() < 2 || positional.len() > 3 {
        return Err("用法: tapecpy write-random <大小> <磁带路径> [选择器] [--seed=N]".into());
    }
    let size = parse_byte_size(positional[0])?;
    if size == 0 {
        return Err("随机流大小必须大于 0".into());
    }
    let seed = seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    });
    let tape_path = positional[1];
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, positional.get(2).copied())?;
    eprintln!("流式随机测试源: {size} 字节，seed={seed}");
    let mut observer = write_cli_observer();
    if let Some(point) = failpoint {
        eprintln!("警告：已启用破坏性 write failpoint {point:?}");
    }
    if let Some(point) = cancelpoint {
        eprintln!("测试：将在安全边界 {point:?} 请求取消");
    }
    let cancellation = cancelpoint.map(|_| app::CancellationToken::default());
    let result = app::WriteSession::new(drive)
        .run_pseudorandom_detailed(
            size,
            seed,
            tape_path,
            app::WriteOptions {
                verification: if failpoint == Some(app::WriteFailpoint::BeforeVerify) {
                    app::WriteVerification::ReadBackSha256
                } else {
                    app::WriteVerification::None
                },
                failpoint,
                cancellation,
                cancelpoint,
            },
            &mut observer,
        )
        .map_err(|error| error.to_string())?;
    print_write_result(tape_path, &result);
    Ok(())
}

fn write_cli_observer() -> impl FnMut(&app::WriteEvent) {
    let mut last_completed = 0usize;
    move |event: &app::WriteEvent| match event.phase {
        app::WritePhase::Preparing => eprintln!(
            "准备写入 {} 个文件 / {} 字节",
            event.files_total, event.bytes_total
        ),
        app::WritePhase::WritingData => {
            if let Some(sample) = &event.telemetry {
                match sample.worst_rate {
                    Some(rate) => eprintln!(
                        "  遥测 t={:.1}s p{}b{} speed={:.1} MiB/s worst={rate:.2}",
                        sample.elapsed_millis as f64 / 1000.0,
                        sample.partition,
                        sample.logical_block,
                        sample.throughput_bytes_per_second / (1024.0 * 1024.0)
                    ),
                    None => eprintln!(
                        "  遥测 t={:.1}s p{}b{} speed={:.1} MiB/s worst=n/a",
                        sample.elapsed_millis as f64 / 1000.0,
                        sample.partition,
                        sample.logical_block,
                        sample.throughput_bytes_per_second / (1024.0 * 1024.0)
                    ),
                }
                return;
            }
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
        app::WritePhase::UpdatingCoherency => {
            eprintln!("正在更新 MAM Volume Coherency Information……")
        }
        app::WritePhase::Verifying => {
            if let Some(path) = &event.current_file {
                eprintln!("正在从磁带回读校验: {path}");
            }
        }
        app::WritePhase::Failed => {
            if let Some(failure) = &event.failure {
                eprintln!(
                    "写入失败：phase={:?} commit={:?} safe_to_retry={} requires_diagnosis={}",
                    failure.phase,
                    failure.commit_state,
                    failure.safe_to_retry,
                    failure.requires_diagnosis
                );
                if failure.cancelled {
                    eprintln!("结果类型：用户取消");
                }
            }
        }
        app::WritePhase::Completed => eprintln!("写入会话已安全完成。"),
    }
}

fn print_write_result(tape_path: &str, result: &app::WriteResult) {
    println!(
        "已写入 {} 个文件 / {} 个目录 / {} 字节 -> {} (highest uid {}, index gen {})",
        result.files,
        result.directories,
        result.bytes,
        tape_path,
        result.file_uid,
        result.generation
    );
    for hash in &result.hashes {
        println!("SHA-256 {}  {}", hash.sha256, hash.path);
    }
    if result.verification == app::WriteVerification::ReadBackSha256 {
        println!("写后回读校验通过。")
    }
    println!(
        "本次设备计数变化: corrected-write={} hard-write={} corrected-read={} hard-read={}",
        display_counter(result.health_delta.corrected_write_errors),
        display_counter(result.health_delta.hard_write_errors),
        display_counter(result.health_delta.corrected_read_errors),
        display_counter(result.health_delta.hard_read_errors),
    );
    if result.health_delta.write_channel_rates.is_empty() {
        println!("LTFSCopyGUI 通道错误率: n/a（本次没有可比较的通道样本）");
    } else {
        for rate in &result.health_delta.write_channel_rates {
            match rate.log10_bit_error_rate {
                Some(value) => println!("  Channel {}: {:.2}", rate.channel, value),
                None => println!("  Channel {}: n/a", rate.channel),
            }
        }
        match result.health_delta.worst_write_channel_rate {
            Some(value) => println!("LTFSCopyGUI 最大通道错误率指数: {value:.2}"),
            None => println!("LTFSCopyGUI 最大通道错误率指数: n/a"),
        }
    }
    if !result.health_delta.active_tape_alerts.is_empty() {
        eprintln!(
            "警告: 写入完成时存在 TapeAlert flags: {}",
            result
                .health_delta
                .active_tape_alerts
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for warning in result
        .health_before
        .warnings
        .iter()
        .chain(&result.health_after.warnings)
    {
        eprintln!("遥测警告: {warning}");
    }
    for warning in &result.telemetry_warnings {
        eprintln!("周期遥测警告: {warning}");
    }
    println!(
        "通道错误率历史: {} samples（容量 {}，默认显示 {}）",
        result.telemetry_history.len(),
        app::CHANNEL_HISTORY_CAPACITY,
        app::CHANNEL_DEFAULT_VISIBLE_SAMPLES
    );
    if let Some(worst) = &result.session_worst_channel_rate {
        println!(
            "会话最差: {:.2} channel {} @ {:.1}s p{}b{} ({})",
            worst.rate,
            worst.channel,
            worst.elapsed_millis as f64 / 1000.0,
            worst.partition,
            worst.logical_block,
            worst.timestamp
        );
    }
}

fn parse_byte_size(text: &str) -> Result<u64, String> {
    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let number = text[..split]
        .parse::<u64>()
        .map_err(|_| format!("无效大小: {text}"))?;
    let multiplier = match text[split..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "mib" => 1024_u64.pow(2),
        "gib" => 1024_u64.pow(3),
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        _ => return Err(format!("不支持的大小单位: {text}")),
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| format!("大小溢出: {text}"))
}

fn cmd_format(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|arg| arg == "--force") {
        return Err("format 会销毁当前磁带上的全部数据；确认后请重新执行并加上 --force".into());
    }
    let positional: Vec<&str> = args
        .iter()
        .filter(|arg| arg.as_str() != "--force")
        .map(String::as_str)
        .collect();
    if positional.len() < 2 || positional.len() > 3 {
        return Err("用法: tapecpy format <6位Barcode> <Volume Name> [选择器] --force".into());
    }
    let options = app::FormatOptions::new(positional[0], positional[1]);
    options.validate()?;
    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, positional.get(2).copied())?;
    eprintln!(
        "警告：正在销毁并重新格式化 {} 中的磁带；Barcode={}，Volume Name={}",
        drive.sg_path.display(),
        options.barcode,
        options.volume_name
    );
    let mut observer = |event: &app::FormatEvent| {
        eprintln!("[{:?}] {}", event.phase, event.message);
    };
    let result = app::FormatSession::new(drive).run(&options, &mut observer)?;
    println!(
        "LTFS format 完成: Barcode={} Volume Name={} UUID={} generation={}",
        result.barcode, result.volume_name, result.volume_uuid, result.generation
    );
    Ok(())
}

fn cmd_erase(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|arg| arg == "--force") {
        return Err("erase 会销毁当前磁带上的数据；确认后请重新执行并加上 --force".into());
    }
    let positional: Vec<&str> = args
        .iter()
        .filter(|arg| arg.as_str() != "--force")
        .map(String::as_str)
        .collect();
    if positional.is_empty() || positional.len() > 2 {
        return Err("用法: tapecpy erase <short|long|minimum> [选择器] --force".into());
    }
    let mode = match positional[0] {
        "short" => app::EraseMode::Short,
        "long" => app::EraseMode::Long,
        "minimum" | "minimum-long" => app::EraseMode::MinimumPartitionLong,
        other => {
            return Err(format!(
                "未知 erase 模式 `{other}`；应为 short、long 或 minimum"
            ));
        }
    };

    let drives = app::discover_drives().map_err(|e| e.to_string())?;
    let drive = app::select_drive(&drives, positional.get(1).copied())?;
    eprintln!(
        "警告：正在对 {} 中的磁带执行 {} erase，现有数据将被销毁",
        drive.sg_path.display(),
        mode.cli_name()
    );
    let mut observer = |event: &app::EraseEvent| {
        if let Some(progress) = event.progress {
            eprintln!(
                "[{:?}] {}（{:.1}%）",
                event.phase,
                event.message,
                progress as f64 * 100.0 / u16::MAX as f64
            );
        } else {
            eprintln!("[{:?}] {}", event.phase, event.message);
        }
    };
    let result = app::EraseSession::new(drive).run(mode, &mut observer)?;
    println!(
        "{} erase 完成，耗时 {} 秒",
        result.mode.cli_name(),
        result.elapsed_seconds
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
    if b { "是" } else { "否" }
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
