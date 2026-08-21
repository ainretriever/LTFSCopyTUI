//! Ratatui presentation layer for the Milestone 12 read-only device workflow.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event as InputEvent, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap};

use crate::app::{
    self, ChannelTelemetryFrame, ChannelTelemetryTracker, DeviceSnapshot, MediaLifecycle,
};
use crate::device::TapeDrive;
use crate::job::{self, CompletionAction, EjectStatus, JobPhase, JobState, VerificationStatus};

const MIN_WIDTH: u16 = 100;
const MIN_HEIGHT: u16 = 35;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Ltfs,
    Sequential,
    Health,
    Jobs,
    JobCompletion,
    ErrorDetails,
    WriteSource,
    ReadRestore,
    Format,
    Erase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatView {
    Editing,
    Confirm,
    Running,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatField {
    VolumeSerial,
    VolumeName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EraseView {
    SelectMode,
    Confirm,
    Running,
    Complete,
}

enum FileCommand {
    ListMounts,
    Browse(PathBuf),
    Scan(Vec<PathBuf>, Option<u64>),
    Stop,
}

enum FileEvent {
    Mounts(Vec<app::MountedFilesystem>),
    Directory(PathBuf, Vec<app::BrowserEntry>),
    Plan(app::SourcePlan, app::CapacityAssessment),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceView {
    Mounts,
    Directory,
    Plan,
    LtfsDestination,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadView {
    TapeBrowser,
    Plan,
    Mounts,
    Destination,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequentialMode {
    RawWrite,
    TarWrite,
    RawRead,
    TarRead,
}

impl SequentialMode {
    fn is_write(self) -> bool {
        matches!(self, Self::RawWrite | Self::TarWrite)
    }

    fn extension(self) -> &'static str {
        if self == Self::TarRead { "tar" } else { "raw" }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequentialView {
    Menu,
    Mounts,
    Directory,
    Filename,
    Confirm,
}

#[derive(Debug, Clone)]
struct TapeBrowserEntry {
    name: String,
    path: String,
    directory: bool,
    size: u64,
}

enum WorkerCommand {
    Discover,
    Select(usize),
    Refresh,
    ReadLtfs,
    Load,
    LoadUnthreaded,
    Unthread,
    Unload,
    Format(app::FormatOptions),
    Erase(app::EraseMode),
    AssessSequential(Option<PathBuf>),
    Suspend(Sender<()>),
    Resume,
    Stop,
}

enum JobCommand {
    Cancel(job::JobId),
    Stop,
}

enum JobEvent {
    States(Vec<JobState>),
    Error(String),
}

enum WorkerEvent {
    Busy(&'static str),
    Drives(Vec<TapeDrive>),
    Snapshot(Box<DeviceSnapshot>, ChannelTelemetryFrame, SnapshotScope),
    Telemetry(Box<app::DriveHealth>, ChannelTelemetryFrame, String),
    TelemetryUnavailable(ChannelTelemetryFrame, String, String),
    FormatProgress(app::FormatEvent),
    FormatCompleted(
        app::FormatResult,
        Box<DeviceSnapshot>,
        ChannelTelemetryFrame,
    ),
    FormatFailed(String),
    EraseProgress(app::EraseEvent),
    EraseCompleted(app::EraseResult, Box<DeviceSnapshot>, ChannelTelemetryFrame),
    EraseFailed(String, Box<DeviceSnapshot>, ChannelTelemetryFrame),
    SequentialAssessment(app::RawMamAssessment, Option<app::RecoverySpaceAssessment>),
    Status(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerOwnershipState {
    Active,
    Suspending,
    Suspended,
}

impl WorkerOwnershipState {
    fn allows_device_access(self) -> bool {
        self == Self::Active
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotScope {
    Basic,
    Ltfs,
}

struct UiState {
    drives: Vec<TapeDrive>,
    drive_index: usize,
    selected: bool,
    page: Page,
    snapshot: Option<DeviceSnapshot>,
    ltfs_read: bool,
    ltfs_open_pending: bool,
    channels: ChannelTelemetryFrame,
    busy: Option<&'static str>,
    status: String,
    last_error: Option<String>,
    jobs: Vec<JobState>,
    job_index: usize,
    cancel_confirm: bool,
    source_view: SourceView,
    read_view: ReadView,
    read_tape_directory: String,
    read_tape_entries: Vec<TapeBrowserEntry>,
    selected_tape_paths: Vec<String>,
    read_plan: Option<app::ReadPlan>,
    read_destination: Option<PathBuf>,
    mounts: Vec<app::MountedFilesystem>,
    browser_path: Option<PathBuf>,
    browser_entries: Vec<app::BrowserEntry>,
    browser_index: usize,
    selected_source_roots: Vec<PathBuf>,
    source_plan: Option<app::SourcePlan>,
    capacity: Option<app::CapacityAssessment>,
    file_busy: bool,
    tape_directory: String,
    tape_directories: Vec<String>,
    tape_target: Option<String>,
    capacity_acknowledged: bool,
    read_back_verify: bool,
    completion_action: CompletionAction,
    start_confirm: bool,
    format_view: FormatView,
    format_field: FormatField,
    format_volume_serial: String,
    format_volume_name: String,
    format_media_id: Option<String>,
    format_message: String,
    format_result: Option<app::FormatResult>,
    erase_view: EraseView,
    erase_mode: app::EraseMode,
    erase_message: String,
    erase_progress: Option<u16>,
    erase_result: Option<app::EraseResult>,
    sequential_view: SequentialView,
    sequential_mode: Option<SequentialMode>,
    sequential_path: Option<PathBuf>,
    sequential_filename: String,
    sequential_overwrite_ack: bool,
    sequential_mam: Option<app::RawMamAssessment>,
    sequential_space: Option<app::RecoverySpaceAssessment>,
    quit: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            drives: Vec::new(),
            drive_index: 0,
            selected: false,
            page: Page::Overview,
            snapshot: None,
            ltfs_read: false,
            ltfs_open_pending: false,
            channels: ChannelTelemetryFrame::default(),
            busy: Some("Discovering tape drives"),
            status: "Starting Milestone 12 TUI".into(),
            last_error: None,
            jobs: Vec::new(),
            job_index: 0,
            cancel_confirm: false,
            source_view: SourceView::Mounts,
            read_view: ReadView::TapeBrowser,
            read_tape_directory: "/".into(),
            read_tape_entries: Vec::new(),
            selected_tape_paths: Vec::new(),
            read_plan: None,
            read_destination: None,
            mounts: Vec::new(),
            browser_path: None,
            browser_entries: Vec::new(),
            browser_index: 0,
            selected_source_roots: Vec::new(),
            source_plan: None,
            capacity: None,
            file_busy: false,
            tape_directory: "/".into(),
            tape_directories: Vec::new(),
            tape_target: None,
            capacity_acknowledged: false,
            read_back_verify: false,
            completion_action: CompletionAction::KeepLoaded,
            start_confirm: false,
            format_view: FormatView::Editing,
            format_field: FormatField::VolumeSerial,
            format_volume_serial: String::new(),
            format_volume_name: String::new(),
            format_media_id: None,
            format_message: String::new(),
            format_result: None,
            erase_view: EraseView::SelectMode,
            erase_mode: app::EraseMode::Short,
            erase_message: String::new(),
            erase_progress: None,
            erase_result: None,
            sequential_view: SequentialView::Menu,
            sequential_mode: None,
            sequential_path: None,
            sequential_filename: String::new(),
            sequential_overwrite_ack: false,
            sequential_mam: None,
            sequential_space: None,
            quit: false,
        }
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<(Self, Terminal<CrosstermBackend<Stdout>>), String> {
        enable_raw_mode().map_err(|error| format!("启用 terminal raw mode 失败: {error}"))?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(format!("进入 alternate screen 失败: {error}"));
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))
            .map_err(|error| format!("初始化 TUI terminal 失败: {error}"))?;
        Ok((Self, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

pub fn run() -> Result<(), String> {
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let _worker = thread::Builder::new()
        .name("tapecpy-device-worker".into())
        .spawn(move || device_worker(command_rx, event_tx))
        .map_err(|error| format!("启动设备 worker 失败: {error}"))?;
    let (job_command_tx, job_command_rx) = mpsc::channel();
    let (job_event_tx, job_event_rx) = mpsc::channel();
    let _job_worker = thread::Builder::new()
        .name("tapecpy-job-monitor".into())
        .spawn(move || job_monitor(job_command_rx, job_event_tx))
        .map_err(|error| format!("启动 job monitor 失败: {error}"))?;
    let (file_command_tx, file_command_rx) = mpsc::channel();
    let (file_event_tx, file_event_rx) = mpsc::channel();
    let _file_worker = thread::Builder::new()
        .name("tapecpy-file-browser".into())
        .spawn(move || file_worker(file_command_rx, file_event_tx))
        .map_err(|error| format!("启动文件浏览 worker 失败: {error}"))?;

    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut state = UiState::default();
    command_tx
        .send(WorkerCommand::Discover)
        .map_err(|_| "设备 worker 已退出".to_string())?;

    while !state.quit {
        while let Ok(message) = event_rx.try_recv() {
            apply_worker_event(&mut state, message);
        }
        while let Ok(message) = job_event_rx.try_recv() {
            apply_job_event(&mut state, message, &command_tx);
        }
        while let Ok(message) = file_event_rx.try_recv() {
            apply_file_event(&mut state, message);
        }
        terminal
            .draw(|frame| render(frame, &state))
            .map_err(|error| format!("绘制 TUI 失败: {error}"))?;
        if event::poll(Duration::from_millis(100))
            .map_err(|error| format!("读取 terminal event 失败: {error}"))?
            && let InputEvent::Key(key) =
                event::read().map_err(|error| format!("读取按键失败: {error}"))?
            && key.kind == KeyEventKind::Press
        {
            handle_key(
                &mut state,
                key.code,
                &command_tx,
                &job_command_tx,
                &file_command_tx,
            );
        }
    }

    let _ = command_tx.send(WorkerCommand::Stop);
    let _ = job_command_tx.send(JobCommand::Stop);
    let _ = file_command_tx.send(FileCommand::Stop);
    // 设备操作可能处于不可中断的 SG_IO 中。退出时让进程终止 worker，避免等待
    // 最长 1800 秒的设备超时；TerminalGuard 会先恢复终端状态。
    Ok(())
}

fn file_worker(commands: Receiver<FileCommand>, events: Sender<FileEvent>) {
    while let Ok(command) = commands.recv() {
        let result = match command {
            FileCommand::ListMounts => app::selectable_filesystems().map(FileEvent::Mounts),
            FileCommand::Browse(path) => {
                app::browse_directory(&path).map(|entries| FileEvent::Directory(path, entries))
            }
            FileCommand::Scan(paths, remaining_capacity_mib) => {
                app::scan_source_roots(&paths).map(|plan| {
                    let capacity = app::assess_write_capacity(
                        plan.payload_bytes,
                        remaining_capacity_mib,
                        job::timestamp_now(),
                    );
                    FileEvent::Plan(plan, capacity)
                })
            }
            FileCommand::Stop => break,
        };
        match result {
            Ok(event) => {
                let _ = events.send(event);
            }
            Err(error) => {
                let _ = events.send(FileEvent::Error(error));
            }
        }
    }
}

fn apply_file_event(state: &mut UiState, event: FileEvent) {
    if state.page == Page::Sequential {
        state.file_busy = false;
        match event {
            FileEvent::Mounts(mounts) => {
                state.mounts = mounts;
                state.browser_index = 0;
                state.sequential_view = SequentialView::Mounts;
                state.status = "Select a mounted filesystem".into();
            }
            FileEvent::Directory(path, entries) => {
                state.browser_path = Some(path);
                state.browser_entries = entries;
                state.browser_index = 0;
                state.sequential_view = SequentialView::Directory;
                state.status = if state.sequential_mode.is_some_and(SequentialMode::is_write) {
                    "Select one source with Space".into()
                } else {
                    "Press S to use this directory for the recovery image".into()
                };
            }
            FileEvent::Error(error) => state.status = error,
            FileEvent::Plan(_, _) => state.status = "Unexpected source scan result".into(),
        }
        return;
    }
    state.file_busy = false;
    if state.page == Page::ReadRestore {
        match event {
            FileEvent::Mounts(mounts) => {
                state.mounts = mounts;
                state.browser_index = 0;
                state.read_view = ReadView::Mounts;
                state.status = "Select the filesystem containing the restore destination".into();
            }
            FileEvent::Directory(path, entries) => {
                state.browser_path = Some(path);
                state.browser_entries = entries
                    .into_iter()
                    .filter(|entry| entry.kind == app::BrowserEntryKind::Directory)
                    .collect();
                state.browser_index = 0;
                state.read_view = ReadView::Destination;
                state.status =
                    "Browse with Enter; press S to select this destination directory".into();
            }
            FileEvent::Error(error) => {
                state.last_error = Some(error.clone());
                state.status = error;
            }
            FileEvent::Plan(_, _) => {
                state.status = "Unexpected Write scan result during Read workflow".into();
            }
        }
        return;
    }
    match event {
        FileEvent::Mounts(mounts) => {
            state.mounts = mounts;
            state.browser_index = 0;
            state.source_view = SourceView::Mounts;
            state.status = "Select a mounted filesystem; network mounts are listed first".into();
        }
        FileEvent::Directory(path, entries) => {
            state.browser_path = Some(path);
            state.browser_entries = entries;
            state.browser_index = 0;
            state.source_view = SourceView::Directory;
            state.status = "Space toggles a source; S scans all selected sources".into();
        }
        FileEvent::Plan(plan, capacity) => {
            state.source_plan = Some(plan);
            state.capacity = Some(capacity);
            state.source_view = SourceView::Plan;
            state.status = "Source plan frozen; LTFS destination selection is the next step".into();
        }
        FileEvent::Error(error) => {
            state.last_error = Some(error.clone());
            state.status = error;
        }
    }
}

fn job_monitor(commands: Receiver<JobCommand>, events: Sender<JobEvent>) {
    let root = match job::default_job_root() {
        Ok(root) => root,
        Err(error) => {
            let _ = events.send(JobEvent::Error(error));
            return;
        }
    };
    loop {
        match job::list_states(&root) {
            Ok(states) => {
                let _ = events.send(JobEvent::States(states));
            }
            Err(error) => {
                let _ = events.send(JobEvent::Error(error));
            }
        }
        match commands.recv_timeout(Duration::from_secs(1)) {
            Ok(JobCommand::Cancel(id)) => {
                if let Err(error) = job::cancel(&root, &id) {
                    let _ = events.send(JobEvent::Error(error));
                }
            }
            Ok(JobCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn device_worker(commands: Receiver<WorkerCommand>, events: Sender<WorkerEvent>) {
    let mut drives = Vec::new();
    let mut selected = None;
    let mut channel_tracker = ChannelTelemetryTracker::default();
    let mut last_telemetry = Instant::now();
    let mut ownership = WorkerOwnershipState::Active;
    loop {
        let wait = if selected.is_some() {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(60)
        };
        match commands.recv_timeout(wait) {
            Ok(WorkerCommand::Discover) => {
                let _ = events.send(WorkerEvent::Busy("Discovering tape drives"));
                match app::discover_drives() {
                    Ok(found) => {
                        drives = found.clone();
                        selected = None;
                        let _ = events.send(WorkerEvent::Drives(found));
                    }
                    Err(error) => {
                        let _ = events.send(WorkerEvent::Error(error.to_string()));
                    }
                }
            }
            Ok(WorkerCommand::Select(index)) => {
                let Some(drive) = drives.get(index).cloned() else {
                    let _ = events.send(WorkerEvent::Error("选择的磁带机不存在".into()));
                    continue;
                };
                selected = Some(drive.clone());
                channel_tracker = ChannelTelemetryTracker::default();
                if ownership.allows_device_access() {
                    with_worker_lease(&drive, "select-basic-refresh", &events, || {
                        refresh_basic_snapshot(&drive, &mut channel_tracker, &events);
                    });
                } else {
                    let _ = events.send(WorkerEvent::Status(
                        "Device worker is suspended; no device access performed".into(),
                    ));
                }
                last_telemetry = Instant::now();
            }
            Ok(WorkerCommand::Refresh) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    with_worker_lease(drive, "basic-refresh", &events, || {
                        refresh_basic_snapshot(drive, &mut channel_tracker, &events);
                    });
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Refresh");
                }
            }
            Ok(WorkerCommand::ReadLtfs) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    with_worker_lease(drive, "read-ltfs", &events, || {
                        read_ltfs_snapshot(drive, &mut channel_tracker, &events);
                    });
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Read LTFS");
                }
            }
            Ok(WorkerCommand::Load) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    run_cartridge_action(
                        drive,
                        &mut channel_tracker,
                        &events,
                        "load-threaded",
                        "Loading and threading cartridge",
                        MediaLifecycle::LoadedThreaded,
                        app::load_tape,
                    );
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Load");
                }
            }
            Ok(WorkerCommand::LoadUnthreaded) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    run_cartridge_action(
                        drive,
                        &mut channel_tracker,
                        &events,
                        "load-unthreaded",
                        "Loading cartridge without threading",
                        MediaLifecycle::PresentUnthreaded,
                        app::load_tape_unthreaded,
                    );
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Load Unthreaded");
                }
            }
            Ok(WorkerCommand::Unthread) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    run_cartridge_action(
                        drive,
                        &mut channel_tracker,
                        &events,
                        "unthread",
                        "Unthreading cartridge without ejecting",
                        MediaLifecycle::PresentUnthreaded,
                        app::unthread_tape,
                    );
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Unthread");
                }
            }
            Ok(WorkerCommand::Unload) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    run_cartridge_action(
                        drive,
                        &mut channel_tracker,
                        &events,
                        "eject",
                        "Ejecting cartridge",
                        MediaLifecycle::NoMediaDetected,
                        app::unload_tape,
                    );
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Unload");
                }
            }
            Ok(WorkerCommand::Format(options)) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    let acquired = with_worker_lease(drive, "format", &events, || {
                        let mut observer = |event: &app::FormatEvent| {
                            let _ = events.send(WorkerEvent::FormatProgress(event.clone()));
                        };
                        match app::FormatSession::new(drive).run(&options, &mut observer) {
                            Ok(result) => {
                                let snapshot = app::inspect_device_snapshot(drive);
                                let channels = channel_tracker
                                    .observe(snapshot.health.as_ref(), &snapshot.refreshed_at);
                                let _ = events.send(WorkerEvent::FormatCompleted(
                                    result,
                                    Box::new(snapshot),
                                    channels,
                                ));
                            }
                            Err(error) => {
                                let _ = events.send(WorkerEvent::FormatFailed(error));
                            }
                        }
                    });
                    if !acquired {
                        let _ = events.send(WorkerEvent::FormatFailed(
                            "Device lease unavailable; format was not started".into(),
                        ));
                    }
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Format");
                }
            }
            Ok(WorkerCommand::Erase(mode)) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    let acquired = with_worker_lease(drive, "erase", &events, || {
                        let mut observer = |event: &app::EraseEvent| {
                            let _ = events.send(WorkerEvent::EraseProgress(event.clone()));
                        };
                        let result = app::EraseSession::new(drive).run(mode, &mut observer);
                        // Erase invalidates any LTFS snapshot. Only refresh basic device/MAM
                        // state here; a later explicit I command may attempt LTFS discovery.
                        let snapshot = app::inspect_device_snapshot_basic(drive);
                        let channels = channel_tracker
                            .observe(snapshot.health.as_ref(), &snapshot.refreshed_at);
                        match result {
                            Ok(result) => {
                                let _ = events.send(WorkerEvent::EraseCompleted(
                                    result,
                                    Box::new(snapshot),
                                    channels,
                                ));
                            }
                            Err(error) => {
                                let _ = events.send(WorkerEvent::EraseFailed(
                                    error,
                                    Box::new(snapshot),
                                    channels,
                                ));
                            }
                        }
                    });
                    if !acquired {
                        let snapshot = app::pending_device_snapshot(drive);
                        let _ = events.send(WorkerEvent::EraseFailed(
                            "Device lease unavailable; erase was not started".into(),
                            Box::new(snapshot),
                            ChannelTelemetryFrame::default(),
                        ));
                    }
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Erase");
                }
            }
            Ok(WorkerCommand::AssessSequential(destination)) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    let _ = events.send(WorkerEvent::Busy("Reading MAM and checking destination"));
                    let result = app::assess_raw_overwrite(drive).and_then(|mam| {
                        let space = destination
                            .as_deref()
                            .map(|path| app::assess_recovery_space(drive, path))
                            .transpose()?;
                        Ok((mam, space))
                    });
                    match result {
                        Ok((mam, space)) => {
                            let _ = events.send(WorkerEvent::SequentialAssessment(mam, space));
                        }
                        Err(error) => {
                            let _ = events.send(WorkerEvent::Error(error));
                        }
                    }
                }
            }
            Ok(WorkerCommand::Suspend(acknowledge)) => {
                ownership = WorkerOwnershipState::Suspending;
                debug_assert_eq!(ownership, WorkerOwnershipState::Suspending);
                // Worker commands are serialized. Reaching this point proves every earlier
                // device command completed and its per-command DeviceLease was dropped.
                ownership = WorkerOwnershipState::Suspended;
                let _ = acknowledge.send(());
            }
            Ok(WorkerCommand::Resume) => {
                ownership = WorkerOwnershipState::Active;
                last_telemetry = Instant::now();
            }
            Ok(WorkerCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        if ownership.allows_device_access()
            && let Some(drive) = selected.as_ref()
            && last_telemetry.elapsed() >= app::CHANNEL_SAMPLE_INTERVAL
        {
            match crate::device::lease::DeviceLease::try_acquire(
                &drive.serial,
                crate::device::lease::LeaseOwner::new("tui-worker", "telemetry"),
            ) {
                Ok(_lease) => match app::read_drive_health(drive) {
                    Ok(health) => {
                        let timestamp = timestamp_now();
                        let frame = channel_tracker.observe(Some(&health), &timestamp);
                        let _ =
                            events.send(WorkerEvent::Telemetry(Box::new(health), frame, timestamp));
                    }
                    Err(error) => {
                        let frame = channel_tracker.mark_error(error.clone());
                        let _ = events.send(WorkerEvent::Error(error));
                        let _ = events.send(WorkerEvent::TelemetryUnavailable(
                            frame,
                            "Telemetry device query failed".into(),
                            timestamp_now(),
                        ));
                    }
                },
                Err(error) => {
                    let frame = channel_tracker.mark_error(error.clone());
                    let _ = events.send(WorkerEvent::TelemetryUnavailable(
                        frame,
                        format!("Device lease unavailable: {error}"),
                        timestamp_now(),
                    ));
                }
            }
            last_telemetry = Instant::now();
        }
    }
}

fn with_worker_lease(
    drive: &TapeDrive,
    operation: &str,
    events: &Sender<WorkerEvent>,
    action: impl FnOnce(),
) -> bool {
    match crate::device::lease::DeviceLease::try_acquire(
        &drive.serial,
        crate::device::lease::LeaseOwner::new("tui-worker", operation),
    ) {
        Ok(_lease) => {
            action();
            true
        }
        Err(error) => {
            let _ = events.send(WorkerEvent::Error(format!(
                "Device lease unavailable for {operation}: {error}"
            )));
            false
        }
    }
}

fn reject_suspended_command(events: &Sender<WorkerEvent>, command: &str) {
    let _ = events.send(WorkerEvent::Status(format!(
        "{command} rejected: device worker is suspended"
    )));
}

fn run_cartridge_action(
    drive: &TapeDrive,
    tracker: &mut ChannelTelemetryTracker,
    events: &Sender<WorkerEvent>,
    operation: &str,
    busy: &'static str,
    expected: MediaLifecycle,
    action: fn(&TapeDrive) -> Result<(), crate::device::Error>,
) {
    let _ = events.send(WorkerEvent::Busy(busy));
    with_worker_lease(drive, operation, events, || match action(drive) {
        Ok(()) => {
            let snapshot = app::inspect_device_snapshot_basic(drive);
            let actual = snapshot.lifecycle;
            let channels = tracker.observe(snapshot.health.as_ref(), &snapshot.refreshed_at);
            let _ = events.send(WorkerEvent::Snapshot(
                Box::new(snapshot),
                channels,
                SnapshotScope::Basic,
            ));
            if actual == expected {
                let _ = events.send(WorkerEvent::Status(format!(
                    "{operation} completed: {}",
                    lifecycle_label(actual)
                )));
            } else {
                let _ = events.send(WorkerEvent::Error(format!(
                    "{operation} returned success, but media state is {} (expected {})",
                    lifecycle_label(actual),
                    lifecycle_label(expected)
                )));
            }
        }
        Err(error) => {
            let _ = events.send(WorkerEvent::Error(error.to_string()));
        }
    });
}

fn refresh_basic_snapshot(
    drive: &TapeDrive,
    tracker: &mut ChannelTelemetryTracker,
    events: &Sender<WorkerEvent>,
) {
    let _ = events.send(WorkerEvent::Busy("Refreshing basic device state"));
    let snapshot = app::inspect_device_snapshot_basic(drive);
    let channels = tracker.observe(snapshot.health.as_ref(), &snapshot.refreshed_at);
    let _ = events.send(WorkerEvent::Snapshot(
        Box::new(snapshot),
        channels,
        SnapshotScope::Basic,
    ));
}

fn read_ltfs_snapshot(
    drive: &TapeDrive,
    tracker: &mut ChannelTelemetryTracker,
    events: &Sender<WorkerEvent>,
) {
    let _ = events.send(WorkerEvent::Busy(
        "Reading LTFS label, index and consistency",
    ));
    let snapshot = app::inspect_device_snapshot(drive);
    let channels = tracker.observe(snapshot.health.as_ref(), &snapshot.refreshed_at);
    let _ = events.send(WorkerEvent::Snapshot(
        Box::new(snapshot),
        channels,
        SnapshotScope::Ltfs,
    ));
}

fn ltfs_probe_error(snapshot: &DeviceSnapshot) -> Option<String> {
    if snapshot.lifecycle != MediaLifecycle::LoadedThreaded {
        return Some("cartridge is not loaded / threaded".into());
    }
    match snapshot.volume.as_ref() {
        Some(volume) if volume.recognized => None,
        Some(volume) => Some(
            volume
                .reason
                .clone()
                .unwrap_or_else(|| "no valid LTFS partition label was found".into()),
        ),
        None => Some(
            snapshot
                .warnings
                .iter()
                .find(|warning| warning.starts_with("LTFS 查询失败:"))
                .cloned()
                .unwrap_or_else(|| "LTFS partition probe returned no volume".into()),
        ),
    }
}

fn timestamp_now() -> String {
    app::ltfs_time_now()
}

fn apply_worker_event(state: &mut UiState, event: WorkerEvent) {
    match event {
        WorkerEvent::Busy(message) => state.busy = Some(message),
        WorkerEvent::Drives(drives) => {
            state.drives = drives;
            state.drive_index = state.drive_index.min(state.drives.len().saturating_sub(1));
            state.busy = None;
            state.last_error = None;
            state.status = format!("{} tape drive(s) discovered", state.drives.len());
        }
        WorkerEvent::Snapshot(snapshot, channels, scope) => {
            let snapshot = *snapshot;
            let ltfs_error = if scope == SnapshotScope::Ltfs {
                ltfs_probe_error(&snapshot)
            } else {
                None
            };
            state.snapshot = Some(snapshot);
            state.channels = channels;
            state.selected = true;
            state.busy = None;
            if ltfs_error.is_none() {
                state.last_error = None;
            }
            state.ltfs_read = scope == SnapshotScope::Ltfs;
            if scope == SnapshotScope::Ltfs && state.ltfs_open_pending {
                state.page = Page::Ltfs;
                state.ltfs_open_pending = false;
            }
            state.status = match (scope, ltfs_error) {
                (SnapshotScope::Basic, _) => {
                    "Basic device state refreshed; LTFS metadata has not been probed".into()
                }
                (SnapshotScope::Ltfs, Some(error)) => {
                    state.last_error = Some(error.clone());
                    format!("LTFS probe failed: {error}")
                }
                (SnapshotScope::Ltfs, None) => {
                    "LTFS partitions, label, index and consistency read completed".into()
                }
            };
        }
        WorkerEvent::Telemetry(health, channels, timestamp) => {
            if let Some(snapshot) = state.snapshot.as_mut() {
                snapshot.health = Some(*health);
            }
            state.channels = channels;
            state.last_error = None;
            state.status = format!("Telemetry refreshed at {}", display_clock(&timestamp));
        }
        WorkerEvent::TelemetryUnavailable(channels, reason, timestamp) => {
            state.channels = channels;
            state.status = format!("Telemetry stale at {}: {reason}", display_clock(&timestamp));
        }
        WorkerEvent::FormatProgress(event) => {
            state.format_view = FormatView::Running;
            state.format_message = format!("{:?}: {}", event.phase, event.message);
            state.status = state.format_message.clone();
            state.busy = None;
        }
        WorkerEvent::FormatCompleted(result, snapshot, channels) => {
            state.format_result = Some(result);
            state.snapshot = Some(*snapshot);
            state.channels = channels;
            state.ltfs_read = true;
            state.format_view = FormatView::Complete;
            state.format_message = "LTFS format completed and generation-1 volume verified".into();
            state.status = state.format_message.clone();
            state.busy = None;
            state.last_error = None;
        }
        WorkerEvent::FormatFailed(error) => {
            state.format_view = FormatView::Complete;
            state.format_message = format!("FORMAT FAILED: {error}");
            state.last_error = Some(error.clone());
            state.status = state.format_message.clone();
            state.busy = None;
        }
        WorkerEvent::EraseProgress(event) => {
            state.erase_view = EraseView::Running;
            state.erase_progress = event.progress;
            state.erase_message = format!("{:?}: {}", event.phase, event.message);
            state.status = state.erase_message.clone();
            state.busy = None;
        }
        WorkerEvent::EraseCompleted(result, snapshot, channels) => {
            state.erase_result = Some(result);
            state.snapshot = Some(*snapshot);
            state.channels = channels;
            state.ltfs_read = false;
            state.erase_view = EraseView::Complete;
            state.erase_progress = None;
            state.erase_message =
                "Erase completed; previous LTFS state was discarded and basic media state refreshed"
                    .into();
            state.status = state.erase_message.clone();
            state.busy = None;
            state.last_error = None;
        }
        WorkerEvent::EraseFailed(error, snapshot, channels) => {
            state.snapshot = Some(*snapshot);
            state.channels = channels;
            state.ltfs_read = false;
            state.erase_view = EraseView::Complete;
            state.erase_progress = None;
            state.erase_message = format!("ERASE FAILED: {error}");
            state.last_error = Some(error.clone());
            state.status = state.erase_message.clone();
            state.busy = None;
        }
        WorkerEvent::SequentialAssessment(mam, space) => {
            state.sequential_mam = Some(mam);
            state.sequential_space = space;
            state.sequential_view = SequentialView::Confirm;
            state.status = "Review MAM, capacity and operation risk before starting".into();
            state.busy = None;
            state.last_error = None;
        }
        WorkerEvent::Status(message) => {
            state.status = message;
            state.busy = None;
            state.last_error = None;
        }
        WorkerEvent::Error(error) => {
            state.ltfs_open_pending = false;
            state.last_error = Some(error.clone());
            state.status = format!("ERROR: {error}");
            state.busy = None;
        }
    }
}

fn apply_job_event(state: &mut UiState, event: JobEvent, device_commands: &Sender<WorkerCommand>) {
    match event {
        JobEvent::States(jobs) => {
            let was_claimed = selected_device_claimed(state);
            state.jobs = jobs;
            state.job_index = state.job_index.min(state.jobs.len().saturating_sub(1));
            let is_claimed = selected_device_claimed(state);
            if is_claimed {
                let (acknowledge, _ignored) = mpsc::channel();
                let _ = device_commands.send(WorkerCommand::Suspend(acknowledge));
                state.status = "Device owned by detached operation; local polling suspended".into();
            } else if was_claimed {
                let _ = device_commands.send(WorkerCommand::Resume);
                state.status =
                    "Detached operation reached a terminal state; telemetry will resume after lease release"
                        .into();
            }
        }
        JobEvent::Error(error) => {
            state.last_error = Some(error.clone());
            state.status = format!("JOB ERROR: {error}");
        }
    }
}

fn selected_device_claimed(state: &UiState) -> bool {
    let Some(serial) = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.drive.serial.as_str())
    else {
        return false;
    };
    state
        .jobs
        .iter()
        .any(|job| job.phase.is_active() && job.spec.drive_serial == serial)
}

fn drive_claiming_job(state: &UiState, serial: &str) -> Option<usize> {
    state
        .jobs
        .iter()
        .position(|job| job.phase.is_active() && job.spec.drive_serial == serial)
}

fn handle_key(
    state: &mut UiState,
    code: KeyCode,
    commands: &Sender<WorkerCommand>,
    job_commands: &Sender<JobCommand>,
    file_commands: &Sender<FileCommand>,
) {
    if state.busy.is_some() {
        return;
    }
    if state.page == Page::WriteSource {
        handle_source_key(state, code, file_commands, commands);
        return;
    }
    if state.page == Page::ReadRestore {
        handle_read_key(state, code, file_commands, commands);
        return;
    }
    if state.page == Page::Sequential {
        handle_sequential_key(state, code, file_commands, commands);
        return;
    }
    if state.page == Page::Jobs {
        handle_job_key(state, code, job_commands);
        return;
    }
    if state.page == Page::JobCompletion {
        if matches!(code, KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc) {
            state.page = Page::Jobs;
        }
        return;
    }
    if state.page == Page::Format {
        handle_format_key(state, code, commands);
        return;
    }
    if state.page == Page::Erase {
        handle_erase_key(state, code, commands);
        return;
    }
    if matches!(code, KeyCode::F(4)) {
        state.page = Page::Jobs;
        return;
    }
    if !state.selected {
        match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => state.quit = true,
            KeyCode::Up | KeyCode::Char('k') => {
                state.drive_index = state.drive_index.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.drive_index =
                    (state.drive_index + 1).min(state.drives.len().saturating_sub(1))
            }
            KeyCode::Enter if !state.drives.is_empty() => {
                let drive = state.drives[state.drive_index].clone();
                if let Some(index) = drive_claiming_job(state, &drive.serial) {
                    state.job_index = index;
                    state.page = Page::Jobs;
                    state.status = "Drive is owned by an active job; attached to job state".into();
                } else {
                    state.selected = true;
                    state.page = Page::Overview;
                    state.snapshot = Some(app::pending_device_snapshot(&drive));
                    state.ltfs_read = false;
                    state.busy = Some("Refreshing basic device state");
                    state.status = "Device opened; use [6] to enter LTFS Operations".into();
                    let _ = commands.send(WorkerCommand::Select(state.drive_index));
                }
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                let _ = commands.send(WorkerCommand::Discover);
            }
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
            if state.page == Page::Overview {
                state.selected = false;
                state.snapshot = None;
            } else {
                state.page = Page::Overview;
            }
        }
        KeyCode::F(1) => state.page = Page::Overview,
        KeyCode::F(3) => state.page = Page::Health,
        KeyCode::Char('1') if state.page == Page::Overview => {
            let available = state
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.lifecycle == MediaLifecycle::NoMediaDetected);
            start_available_cartridge_command(
                state,
                commands,
                available,
                WorkerCommand::LoadUnthreaded,
                "Load Unthreaded",
            );
        }
        KeyCode::Char('2') if state.page == Page::Overview => {
            let available = state.snapshot.as_ref().is_some_and(|snapshot| {
                matches!(
                    snapshot.lifecycle,
                    MediaLifecycle::NoMediaDetected | MediaLifecycle::PresentUnthreaded
                )
            });
            start_available_cartridge_command(
                state,
                commands,
                available,
                WorkerCommand::Load,
                "Load & Thread",
            );
        }
        KeyCode::Char('3') if state.page == Page::Overview => {
            start_cartridge_command(
                state,
                commands,
                MediaLifecycle::LoadedThreaded,
                WorkerCommand::Unthread,
                "Unthread",
            );
        }
        KeyCode::Char('4') if state.page == Page::Overview => {
            let available = state.snapshot.as_ref().is_some_and(|snapshot| {
                matches!(
                    snapshot.lifecycle,
                    MediaLifecycle::PresentUnthreaded | MediaLifecycle::LoadedThreaded
                )
            });
            start_available_cartridge_command(
                state,
                commands,
                available,
                WorkerCommand::Unload,
                "Eject",
            );
        }
        KeyCode::Char('5') if state.page == Page::Overview => open_erase_workflow(state),
        KeyCode::Char('6') if state.page == Page::Overview => {
            if state
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.lifecycle == MediaLifecycle::LoadedThreaded)
            {
                state.ltfs_open_pending = true;
                state.status = "Reading LTFS partitions before opening operations".into();
                if commands.send(WorkerCommand::ReadLtfs).is_err() {
                    state.ltfs_open_pending = false;
                    state.status = "LTFS probe failed: device worker has exited".into();
                }
            } else {
                state.status = "LTFS Operations requires Load & Thread first".into();
            }
        }
        KeyCode::Char('7') if state.page == Page::Overview => {
            if state
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.lifecycle == MediaLifecycle::LoadedThreaded)
            {
                state.page = Page::Sequential;
                state.status = "Select a RAW/TAR sequential workflow".into();
            } else {
                state.status = "Sequential Operations requires Load & Thread first".into();
            }
        }
        KeyCode::Char('1') if state.page == Page::Ltfs => {
            let readable = state
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.volume.as_ref())
                .and_then(|volume| volume.index.as_ref())
                .is_some();
            if readable {
                open_read_browser(state, "/");
                state.page = Page::ReadRestore;
                state.selected_tape_paths.clear();
                state.read_plan = None;
                state.read_destination = None;
                state.status =
                    "Select LTFS files/directories with Space; press S to build the Read Plan"
                        .into();
            } else {
                state.status = "LTFS Read unavailable: no readable LTFS index".into();
            }
        }
        KeyCode::Char('2') | KeyCode::Char('w') | KeyCode::Char('W')
            if state.page == Page::Ltfs =>
        {
            let writable = state.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.lifecycle == MediaLifecycle::LoadedThreaded
                    && snapshot
                        .diagnosis
                        .as_ref()
                        .is_some_and(|diagnosis| diagnosis.safe_for_normal_write)
            });
            if selected_device_claimed(state) {
                state.status = "Write blocked: detached operation owns this drive".into();
            } else if writable {
                state.page = Page::WriteSource;
                state.file_busy = true;
                state.source_plan = None;
                state.capacity = None;
                state.selected_source_roots.clear();
                let _ = file_commands.send(FileCommand::ListMounts);
            } else {
                state.status =
                    "Write requires a loaded, healthy LTFS volume; enter through [6] LTFS Operations"
                        .into();
            }
        }
        KeyCode::Char('3') | KeyCode::Char('f') | KeyCode::Char('F')
            if state.page == Page::Ltfs =>
        {
            open_format_workflow(state);
        }
        KeyCode::Char('e') | KeyCode::Char('E') => {
            open_erase_workflow(state);
        }
        KeyCode::Char('d') | KeyCode::Char('D') if state.last_error.is_some() => {
            state.page = Page::ErrorDetails
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            if selected_device_claimed(state) {
                state.status = "Refresh blocked: detached operation owns this drive".into();
            } else {
                let _ = commands.send(WorkerCommand::Refresh);
            }
        }
        KeyCode::Char('l') | KeyCode::Char('L') => {
            if selected_device_claimed(state) {
                state.status = "Load blocked: detached operation owns this drive".into();
            } else {
                let _ = commands.send(WorkerCommand::Load);
            }
        }
        KeyCode::Char('u') | KeyCode::Char('U') => {
            if selected_device_claimed(state) {
                state.status = "Unload blocked: detached operation owns this drive".into();
            } else {
                let _ = commands.send(WorkerCommand::Unload);
            }
        }
        _ => {}
    }
}

fn start_cartridge_command(
    state: &mut UiState,
    commands: &Sender<WorkerCommand>,
    required: MediaLifecycle,
    command: WorkerCommand,
    label: &str,
) {
    let available = state
        .snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.lifecycle == required);
    start_available_cartridge_command(state, commands, available, command, label);
}

fn start_available_cartridge_command(
    state: &mut UiState,
    commands: &Sender<WorkerCommand>,
    available: bool,
    command: WorkerCommand,
    label: &str,
) {
    if selected_device_claimed(state) {
        state.status = format!("{label} blocked: detached operation owns this drive");
    } else if !available {
        state.status = format!("{label} is unavailable in the current cartridge state");
    } else if commands.send(command).is_err() {
        state.status = format!("{label} failed: device worker has exited");
    }
}

fn open_erase_workflow(state: &mut UiState) {
    if selected_device_claimed(state) {
        state.status = "Erase blocked: detached operation owns this drive".into();
        return;
    }
    let Some(snapshot) = state.snapshot.as_ref() else {
        state.status = "Erase requires a device snapshot".into();
        return;
    };
    if snapshot.lifecycle != MediaLifecycle::LoadedThreaded {
        state.status = "Erase requires loaded / threaded media".into();
        return;
    }
    if snapshot
        .media
        .as_ref()
        .and_then(|media| media.tape_status)
        .is_some_and(|status| status.is_write_protected())
    {
        state.status = "Erase blocked: cartridge is write protected".into();
        return;
    }
    state.erase_view = EraseView::SelectMode;
    state.erase_mode = app::EraseMode::Short;
    state.erase_message = "Select an erase behavior and review its guarantees".into();
    state.erase_progress = None;
    state.erase_result = None;
    state.page = Page::Erase;
}

fn handle_erase_key(state: &mut UiState, code: KeyCode, device_commands: &Sender<WorkerCommand>) {
    match state.erase_view {
        EraseView::Running => {
            state.status = "Erase is running; TUI exit and device commands are disabled".into();
        }
        EraseView::Complete => {
            if matches!(code, KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc) {
                state.page = Page::Overview;
            }
        }
        EraseView::Confirm => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                state.erase_view = EraseView::Running;
                state.erase_progress = None;
                state.erase_message =
                    format!("Starting destructive {} erase", state.erase_mode.cli_name());
                if device_commands
                    .send(WorkerCommand::Erase(state.erase_mode))
                    .is_err()
                {
                    state.erase_view = EraseView::Complete;
                    state.erase_message = "ERASE FAILED: device worker is unavailable".into();
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                state.erase_view = EraseView::SelectMode;
            }
            _ => {}
        },
        EraseView::SelectMode => match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                state.page = Page::Overview;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.erase_mode = match state.erase_mode {
                    app::EraseMode::Short => app::EraseMode::MinimumPartitionLong,
                    app::EraseMode::MinimumPartitionLong => app::EraseMode::Long,
                    app::EraseMode::Long => app::EraseMode::Short,
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.erase_mode = match state.erase_mode {
                    app::EraseMode::Short => app::EraseMode::Long,
                    app::EraseMode::Long => app::EraseMode::MinimumPartitionLong,
                    app::EraseMode::MinimumPartitionLong => app::EraseMode::Short,
                };
            }
            KeyCode::Char('1') => state.erase_mode = app::EraseMode::Short,
            KeyCode::Char('2') => state.erase_mode = app::EraseMode::Long,
            KeyCode::Char('3') => state.erase_mode = app::EraseMode::MinimumPartitionLong,
            KeyCode::Enter => {
                state.erase_view = EraseView::Confirm;
                state.erase_message = format!(
                    "FINAL CONFIRMATION: {} erase destroys access to existing tape data",
                    state.erase_mode.cli_name()
                );
            }
            _ => {}
        },
    }
}

fn open_format_workflow(state: &mut UiState) {
    if selected_device_claimed(state) {
        state.status = "Format blocked: detached operation owns this drive".into();
        return;
    }
    let Some(snapshot) = state.snapshot.as_ref() else {
        state.status = "Format requires a device snapshot".into();
        return;
    };
    if snapshot.lifecycle != MediaLifecycle::LoadedThreaded {
        state.status = "Format requires loaded / threaded media".into();
        return;
    }
    if snapshot
        .media
        .as_ref()
        .and_then(|media| media.tape_status)
        .is_some_and(|status| status.is_write_protected())
    {
        state.status = "Format blocked: cartridge is write protected".into();
        return;
    }
    let media_id = snapshot
        .media
        .as_ref()
        .and_then(|media| media.density_code)
        .and_then(crate::device::density::lto_generation_suffix)
        .map(str::to_owned);
    if media_id.is_none() {
        state.status = "Format blocked: unable to derive LTO Media ID".into();
        return;
    }
    state.format_media_id = media_id;
    state.format_volume_serial = snapshot
        .media
        .as_ref()
        .and_then(|media| media.mam.as_ref())
        .and_then(|mam| mam.barcode.as_deref())
        .map(|barcode| barcode.chars().take(6).collect())
        .unwrap_or_default();
    state.format_volume_name.clear();
    state.format_field = FormatField::VolumeSerial;
    state.format_view = FormatView::Editing;
    state.format_result = None;
    state.format_message = "Enter a six-character Volume Serial and LTFS Volume Name".into();
    state.page = Page::Format;
}

fn handle_format_key(state: &mut UiState, code: KeyCode, device_commands: &Sender<WorkerCommand>) {
    match state.format_view {
        FormatView::Running => {
            state.status = "Format is running; TUI exit and device commands are disabled".into();
        }
        FormatView::Complete => {
            if matches!(code, KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc) {
                state.page = Page::Ltfs;
            }
        }
        FormatView::Confirm => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let options = app::FormatOptions::new(
                    state.format_volume_serial.clone(),
                    state.format_volume_name.clone(),
                );
                state.format_view = FormatView::Running;
                state.format_message = "Starting destructive LTFS format".into();
                if device_commands
                    .send(WorkerCommand::Format(options))
                    .is_err()
                {
                    state.format_view = FormatView::Complete;
                    state.format_message = "FORMAT FAILED: device worker is unavailable".into();
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                state.format_view = FormatView::Editing;
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                state.format_view = FormatView::Editing;
            }
            _ => {}
        },
        FormatView::Editing => match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                state.page = Page::Ltfs;
            }
            KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
                state.format_field = match state.format_field {
                    FormatField::VolumeSerial => FormatField::VolumeName,
                    FormatField::VolumeName => FormatField::VolumeSerial,
                };
            }
            KeyCode::Backspace => match state.format_field {
                FormatField::VolumeSerial => {
                    state.format_volume_serial.pop();
                }
                FormatField::VolumeName => {
                    state.format_volume_name.pop();
                }
            },
            KeyCode::Char(character) => match state.format_field {
                FormatField::VolumeSerial
                    if state.format_volume_serial.len() < 6
                        && character.is_ascii_alphanumeric() =>
                {
                    state
                        .format_volume_serial
                        .push(character.to_ascii_uppercase());
                }
                FormatField::VolumeName
                    if state.format_volume_name.chars().count() < 255
                        && !character.is_control() =>
                {
                    state.format_volume_name.push(character);
                }
                _ => {}
            },
            KeyCode::Enter => {
                let options = app::FormatOptions::new(
                    state.format_volume_serial.clone(),
                    state.format_volume_name.clone(),
                );
                match options.validate() {
                    Ok(()) if state.format_volume_serial.len() == 6 => {
                        state.format_view = FormatView::Confirm;
                        state.format_message =
                            "FINAL CONFIRMATION: formatting destroys all existing tape data".into();
                    }
                    Ok(()) => {
                        state.format_message =
                            "Volume Serial must contain exactly six ASCII letters/digits".into();
                    }
                    Err(error) => state.format_message = error,
                }
            }
            _ => {}
        },
    }
}

fn handle_source_key(
    state: &mut UiState,
    code: KeyCode,
    commands: &Sender<FileCommand>,
    device_commands: &Sender<WorkerCommand>,
) {
    if state.file_busy {
        if matches!(code, KeyCode::Esc) {
            state.status = "The current filesystem request cannot be cancelled yet".into();
        }
        return;
    }
    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') => back_write_level(state),
        KeyCode::Up | KeyCode::Char('k') => {
            state.browser_index = state.browser_index.saturating_sub(1)
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let length = match state.source_view {
                SourceView::Mounts => state.mounts.len(),
                SourceView::Directory => state.browser_entries.len(),
                SourceView::LtfsDestination => state.tape_directories.len() + 1,
                SourceView::Plan | SourceView::Confirm => 0,
            };
            state.browser_index = (state.browser_index + 1).min(length.saturating_sub(1));
        }
        KeyCode::Enter if state.source_view == SourceView::Mounts => {
            if let Some(mount) = state.mounts.get(state.browser_index) {
                state.file_busy = true;
                let _ = commands.send(FileCommand::Browse(mount.mount_point.clone()));
            }
        }
        KeyCode::Enter if state.source_view == SourceView::Directory => {
            if let Some(entry) = state.browser_entries.get(state.browser_index)
                && entry.kind == app::BrowserEntryKind::Directory
            {
                state.file_busy = true;
                let _ = commands.send(FileCommand::Browse(entry.path.clone()));
            }
        }
        KeyCode::Char(' ') if state.source_view == SourceView::Directory => {
            if let Some(entry) = state.browser_entries.get(state.browser_index)
                && matches!(
                    entry.kind,
                    app::BrowserEntryKind::Directory | app::BrowserEntryKind::File
                )
            {
                toggle_source_root(state, entry.path.clone());
            }
        }
        KeyCode::Char(' ') if state.source_view == SourceView::Mounts => {
            if let Some(mount) = state.mounts.get(state.browser_index) {
                toggle_source_root(state, mount.mount_point.clone());
            }
        }
        KeyCode::Char('s') | KeyCode::Char('S')
            if matches!(
                state.source_view,
                SourceView::Directory | SourceView::Mounts
            ) =>
        {
            start_source_scan(state, commands);
        }
        KeyCode::Backspace | KeyCode::Esc if state.source_view == SourceView::Directory => {
            let Some(current) = state.browser_path.as_ref() else {
                return;
            };
            if state
                .mounts
                .iter()
                .any(|mount| mount.mount_point == *current)
            {
                state.source_view = SourceView::Mounts;
                state.browser_index = 0;
            } else if let Some(parent) = current.parent() {
                state.file_busy = true;
                let _ = commands.send(FileCommand::Browse(parent.to_path_buf()));
            }
        }
        KeyCode::Esc if state.source_view == SourceView::Plan => {
            state.source_view = SourceView::Directory;
            state.source_plan = None;
            state.capacity = None;
        }
        KeyCode::Enter if state.source_view == SourceView::Plan => {
            open_tape_directory(state, "/");
        }
        KeyCode::Enter if state.source_view == SourceView::LtfsDestination => {
            if state.browser_index == 0 {
                select_tape_destination(state);
            } else if let Some(name) = state.tape_directories.get(state.browser_index - 1) {
                let path = if state.tape_directory == "/" {
                    format!("/{name}")
                } else {
                    format!("{}/{name}", state.tape_directory)
                };
                open_tape_directory(state, &path);
            }
        }
        KeyCode::Backspace | KeyCode::Esc if state.source_view == SourceView::LtfsDestination => {
            if state.tape_directory == "/" {
                state.source_view = SourceView::Plan;
            } else {
                let parent = state
                    .tape_directory
                    .rsplit_once('/')
                    .map_or(
                        "/",
                        |(parent, _)| if parent.is_empty() { "/" } else { parent },
                    )
                    .to_string();
                open_tape_directory(state, &parent);
            }
        }
        KeyCode::Char('a') | KeyCode::Char('A')
            if state.source_view == SourceView::Confirm
                && state.capacity.as_ref().is_some_and(|capacity| {
                    matches!(
                        capacity.status,
                        app::CapacityStatus::WarningAboveNinetyPercent
                            | app::CapacityStatus::Unknown
                    )
                }) =>
        {
            state.capacity_acknowledged = !state.capacity_acknowledged;
        }
        KeyCode::Char('v') | KeyCode::Char('V')
            if state.source_view == SourceView::Confirm && !state.start_confirm =>
        {
            state.read_back_verify = !state.read_back_verify;
        }
        KeyCode::Char('e') | KeyCode::Char('E')
            if state.source_view == SourceView::Confirm && !state.start_confirm =>
        {
            state.completion_action = match state.completion_action {
                CompletionAction::KeepLoaded => CompletionAction::EjectAfterCommit,
                CompletionAction::EjectAfterCommit => CompletionAction::KeepLoaded,
            };
        }
        KeyCode::Enter if state.source_view == SourceView::Confirm => {
            if state
                .capacity
                .as_ref()
                .is_some_and(|capacity| capacity.status == app::CapacityStatus::BlockedInsufficient)
            {
                state.status = "Start blocked: source exceeds LTFS available capacity".into();
            } else if capacity_requires_ack(state) && !state.capacity_acknowledged {
                state.status = "Press A to acknowledge the capacity warning first".into();
            } else {
                state.start_confirm = true;
            }
        }
        KeyCode::Char('y') | KeyCode::Char('Y')
            if state.source_view == SourceView::Confirm && state.start_confirm =>
        {
            start_write_job(state, device_commands);
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc
            if state.source_view == SourceView::Confirm && state.start_confirm =>
        {
            state.start_confirm = false;
        }
        KeyCode::Esc if state.source_view == SourceView::Confirm => {
            state.source_view = SourceView::LtfsDestination;
            state.tape_target = None;
        }
        _ => {}
    }
}

fn handle_sequential_key(
    state: &mut UiState,
    code: KeyCode,
    files: &Sender<FileCommand>,
    device_commands: &Sender<WorkerCommand>,
) {
    if state.file_busy {
        return;
    }
    match state.sequential_view {
        SequentialView::Menu => match code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => state.page = Page::Overview,
            KeyCode::Char(key @ ('1' | '2' | '3' | '4')) => {
                state.sequential_mode = Some(match key {
                    '1' => SequentialMode::RawWrite,
                    '2' => SequentialMode::TarWrite,
                    '3' => SequentialMode::RawRead,
                    _ => SequentialMode::TarRead,
                });
                state.sequential_path = None;
                state.sequential_overwrite_ack = false;
                state.read_back_verify = false;
                state.file_busy = true;
                let _ = files.send(FileCommand::ListMounts);
            }
            _ => {}
        },
        SequentialView::Mounts => match code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.browser_index = state.browser_index.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.browser_index =
                    (state.browser_index + 1).min(state.mounts.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if let Some(mount) = state.mounts.get(state.browser_index) {
                    state.file_busy = true;
                    let _ = files.send(FileCommand::Browse(mount.mount_point.clone()));
                }
            }
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                state.sequential_view = SequentialView::Menu
            }
            _ => {}
        },
        SequentialView::Directory => match code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.browser_index = state.browser_index.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.browser_index =
                    (state.browser_index + 1).min(state.browser_entries.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if let Some(entry) = state.browser_entries.get(state.browser_index)
                    && entry.kind == app::BrowserEntryKind::Directory
                {
                    state.file_busy = true;
                    let _ = files.send(FileCommand::Browse(entry.path.clone()));
                }
            }
            KeyCode::Char(' ') if state.sequential_mode.is_some_and(SequentialMode::is_write) => {
                if let Some(entry) = state.browser_entries.get(state.browser_index) {
                    let allowed = match state.sequential_mode {
                        Some(SequentialMode::RawWrite) => entry.kind == app::BrowserEntryKind::File,
                        Some(SequentialMode::TarWrite) => matches!(
                            entry.kind,
                            app::BrowserEntryKind::File
                                | app::BrowserEntryKind::Directory
                                | app::BrowserEntryKind::Symlink
                        ),
                        _ => false,
                    };
                    if allowed {
                        state.sequential_path = Some(entry.path.clone());
                        state.sequential_mam = None;
                        state.sequential_space = None;
                        state.busy = Some("Reading MAM and checking destination");
                        let _ = device_commands.send(WorkerCommand::AssessSequential(None));
                    } else {
                        state.status =
                            "This source type is unavailable for the selected mode".into();
                    }
                }
            }
            KeyCode::Char('s') | KeyCode::Char('S')
                if state.sequential_mode.is_some_and(|mode| !mode.is_write()) =>
            {
                if let Some(directory) = state.browser_path.clone() {
                    state.sequential_path = Some(directory);
                    let mode = state.sequential_mode.expect("sequential mode exists");
                    state.sequential_filename = format!("tapecpy-recovery.{}", mode.extension());
                    state.sequential_view = SequentialView::Filename;
                }
            }
            KeyCode::Backspace | KeyCode::Esc => {
                if let Some(current) = state.browser_path.as_ref()
                    && let Some(parent) = current.parent()
                    && !state
                        .mounts
                        .iter()
                        .any(|mount| mount.mount_point == *current)
                {
                    state.file_busy = true;
                    let _ = files.send(FileCommand::Browse(parent.to_path_buf()));
                } else {
                    state.sequential_view = SequentialView::Mounts;
                }
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                state.sequential_view = SequentialView::Mounts
            }
            _ => {}
        },
        SequentialView::Filename => match code {
            KeyCode::Backspace => {
                state.sequential_filename.pop();
            }
            KeyCode::Char(character) if !character.is_control() && character != '/' => {
                state.sequential_filename.push(character)
            }
            KeyCode::Enter if !state.sequential_filename.is_empty() => {
                if let Some(directory) = state.sequential_path.as_ref() {
                    let output = directory.join(&state.sequential_filename);
                    if output.exists() {
                        state.status = format!("Destination already exists: {}", output.display());
                    } else {
                        state.sequential_path = Some(output);
                        state.sequential_mam = None;
                        state.sequential_space = None;
                        state.busy = Some("Reading MAM and checking destination");
                        let _ = device_commands.send(WorkerCommand::AssessSequential(
                            state
                                .sequential_path
                                .as_ref()
                                .and_then(|path| path.parent())
                                .map(|parent| parent.to_path_buf()),
                        ));
                    }
                }
            }
            KeyCode::Esc => state.sequential_view = SequentialView::Directory,
            _ => {}
        },
        SequentialView::Confirm => match code {
            KeyCode::Char('a') | KeyCode::Char('A')
                if state.sequential_mode.is_some_and(SequentialMode::is_write) =>
            {
                state.sequential_overwrite_ack = !state.sequential_overwrite_ack
            }
            KeyCode::Char('v') | KeyCode::Char('V')
                if state.sequential_mode.is_some_and(SequentialMode::is_write) =>
            {
                state.read_back_verify = !state.read_back_verify
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => start_sequential_job(state, device_commands),
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                state.sequential_view = SequentialView::Menu;
                state.sequential_path = None;
            }
            _ => {}
        },
    }
}

fn start_sequential_job(state: &mut UiState, device_commands: &Sender<WorkerCommand>) {
    let (Some(snapshot), Some(mode), Some(path)) = (
        state.snapshot.as_ref(),
        state.sequential_mode,
        state.sequential_path.as_ref(),
    ) else {
        state.status = "Sequential operation is incomplete".into();
        return;
    };
    if mode.is_write() {
        let requires_ack = state.sequential_mam.as_ref().is_none_or(|mam| {
            matches!(
                mam.status,
                app::RawMamStatus::LtfsDetected | app::RawMamStatus::Unknown
            )
        });
        if requires_ack && !state.sequential_overwrite_ack {
            state.status = "Press A to acknowledge the reported MAM overwrite risk first".into();
            return;
        }
    } else if state
        .sequential_space
        .as_ref()
        .is_none_or(|space| !space.sufficient)
    {
        state.status = "Recovery blocked: destination capacity requirement is not satisfied".into();
        return;
    }
    let mount = state
        .mounts
        .iter()
        .filter(|mount| path.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.components().count());
    let host = job::Endpoint {
        path: path.display().to_string(),
        filesystem_type: mount.map(|mount| mount.filesystem_type.clone()),
        mount_source: mount.map(|mount| mount.source.clone()),
    };
    let tape = job::Endpoint {
        path: "tape://partition-0".into(),
        filesystem_type: None,
        mount_source: None,
    };
    let operation = match mode {
        SequentialMode::RawWrite => job::OperationKind::RawWrite,
        SequentialMode::TarWrite => job::OperationKind::TarWrite,
        SequentialMode::RawRead => job::OperationKind::RawRead,
        SequentialMode::TarRead => job::OperationKind::TarRead,
    };
    let (source, destination) = if mode.is_write() {
        (host, tape)
    } else {
        (tape, host)
    };
    let spec = match job::JobSpec::new(
        operation,
        snapshot.drive.sg_path.display().to_string(),
        snapshot.drive.serial.clone(),
        source,
        destination,
        mode.is_write() && state.read_back_verify,
    )
    .with_sequential_options(
        state.sequential_overwrite_ack,
        state.sequential_overwrite_ack,
        512 * 1024,
    ) {
        Ok(spec) => spec,
        Err(error) => {
            state.status = error;
            return;
        }
    };
    let root = match job::default_job_root() {
        Ok(root) => root,
        Err(error) => {
            state.status = error;
            return;
        }
    };
    let (acknowledge, acknowledged) = mpsc::channel();
    if device_commands
        .send(WorkerCommand::Suspend(acknowledge))
        .is_err()
        || acknowledged.recv_timeout(Duration::from_secs(10)).is_err()
    {
        let _ = device_commands.send(WorkerCommand::Resume);
        state.status = "Device worker did not confirm ownership handoff".into();
        return;
    }
    match job::spawn_detached(spec, &root) {
        Ok(job_state) => {
            state.jobs.insert(0, job_state);
            state.job_index = 0;
            state.page = Page::Jobs;
            state.sequential_view = SequentialView::Menu;
            state.status =
                "Detached RAW/TAR operation started; closing TUI will not stop it".into();
        }
        Err(error) => {
            let _ = device_commands.send(WorkerCommand::Resume);
            state.status = format!("Failed to start detached operation: {error}");
        }
    }
}

fn back_write_level(state: &mut UiState) {
    state.start_confirm = false;
    match state.source_view {
        SourceView::Mounts => state.page = Page::Ltfs,
        SourceView::Directory => {
            state.source_view = SourceView::Mounts;
            state.browser_index = 0;
        }
        SourceView::Plan => {
            state.source_view = SourceView::Directory;
            state.source_plan = None;
            state.capacity = None;
        }
        SourceView::LtfsDestination => state.source_view = SourceView::Plan,
        SourceView::Confirm => {
            state.source_view = SourceView::LtfsDestination;
            state.tape_target = None;
        }
    }
}

fn toggle_source_root(state: &mut UiState, path: PathBuf) {
    if let Some(position) = state
        .selected_source_roots
        .iter()
        .position(|selected| selected == &path)
    {
        state.selected_source_roots.remove(position);
        state.status = format!("Removed source {}", path.display());
    } else {
        state.selected_source_roots.push(path.clone());
        state.status = format!(
            "Selected {} ({} roots total)",
            path.display(),
            state.selected_source_roots.len()
        );
    }
}

fn start_source_scan(state: &mut UiState, commands: &Sender<FileCommand>) {
    if state.selected_source_roots.is_empty() {
        state.status = "Select at least one source with Space first".into();
        return;
    }
    let remaining_capacity_mib = state
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.media.as_ref())
        .and_then(|media| media.mam.as_ref())
        .and_then(|mam| mam.remaining_capacity_mib);
    state.file_busy = true;
    state.status = format!(
        "Scanning {} selected source roots",
        state.selected_source_roots.len()
    );
    let _ = commands.send(FileCommand::Scan(
        state.selected_source_roots.clone(),
        remaining_capacity_mib,
    ));
}

fn open_tape_directory(state: &mut UiState, path: &str) {
    let Some(directory) = state
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.volume.as_ref())
        .and_then(|volume| volume.index.as_ref())
        .and_then(|index| index.find_directory(path))
    else {
        state.status = format!("LTFS directory no longer exists: {path}");
        return;
    };
    let mut children = directory
        .entries
        .iter()
        .filter_map(|entry| match entry {
            crate::ltfs::index::DirectoryEntry::Directory(directory) => {
                Some(directory.name.clone())
            }
            crate::ltfs::index::DirectoryEntry::File(_) => None,
        })
        .collect::<Vec<_>>();
    children.sort();
    state.tape_directory = path.to_string();
    state.tape_directories = children;
    state.browser_index = 0;
    state.source_view = SourceView::LtfsDestination;
    state.status = "Select this LTFS directory or browse into a child directory".into();
}

fn select_tape_destination(state: &mut UiState) {
    let Some(plan) = state.source_plan.as_ref() else {
        state.status = "Source plan is unavailable".into();
        return;
    };
    let Some(index) = state
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.volume.as_ref())
        .and_then(|volume| volume.index.as_ref())
    else {
        state.status = "Trusted LTFS index is unavailable".into();
        return;
    };
    match app::validate_write_destinations(index, &plan.roots, &state.tape_directory) {
        Ok(_) => {
            state.tape_target = Some(state.tape_directory.clone());
            state.capacity_acknowledged = false;
            state.start_confirm = false;
            state.source_view = SourceView::Confirm;
            state.status = "Review the complete operation plan before starting".into();
        }
        Err(error) => state.status = error,
    }
}

fn capacity_requires_ack(state: &UiState) -> bool {
    state.capacity.as_ref().is_some_and(|capacity| {
        matches!(
            capacity.status,
            app::CapacityStatus::WarningAboveNinetyPercent | app::CapacityStatus::Unknown
        )
    })
}

fn start_write_job(state: &mut UiState, device_commands: &Sender<WorkerCommand>) {
    let Some(snapshot) = state.snapshot.as_ref() else {
        state.status = "Device snapshot is unavailable".into();
        return;
    };
    let Some(plan) = state.source_plan.as_ref() else {
        state.status = "Source plan is unavailable".into();
        return;
    };
    let Some(capacity) = state.capacity.as_ref() else {
        state.status = "Capacity assessment is unavailable".into();
        return;
    };
    let Some(_) = plan.roots.first() else {
        state.status = "Source root is unavailable".into();
        return;
    };
    let Some(tape_target) = state.tape_target.as_ref() else {
        state.status = "LTFS target is unavailable".into();
        return;
    };
    let source_roots = plan
        .roots
        .iter()
        .map(|root| {
            let mount = state
                .mounts
                .iter()
                .filter(|mount| root.starts_with(&mount.mount_point))
                .max_by_key(|mount| mount.mount_point.components().count());
            job::Endpoint {
                path: root.display().to_string(),
                filesystem_type: mount.map(|mount| mount.filesystem_type.clone()),
                mount_source: mount.map(|mount| mount.source.clone()),
            }
        })
        .collect::<Vec<_>>();
    let source = source_roots
        .first()
        .cloned()
        .expect("source plan is not empty");
    let destination = job::Endpoint {
        path: tape_target.clone(),
        filesystem_type: None,
        mount_source: None,
    };
    let barcode = snapshot.media.as_ref().and_then(|media| {
        media
            .full_label_hint()
            .or_else(|| media.mam.as_ref()?.barcode.clone())
    });
    let volume_name = snapshot
        .volume
        .as_ref()
        .and_then(|volume| volume.index.as_ref())
        .and_then(|index| index.volume_name())
        .map(str::to_owned);
    let spec = match job::JobSpec::new(
        job::OperationKind::Write,
        snapshot.drive.sg_path.display().to_string(),
        snapshot.drive.serial.clone(),
        source,
        destination,
        state.read_back_verify,
    )
    .with_source_roots(source_roots)
    .with_completion(state.completion_action, barcode, volume_name)
    .with_write_preflight(plan, capacity, state.capacity_acknowledged)
    {
        Ok(spec) => spec,
        Err(error) => {
            state.status = error;
            state.start_confirm = false;
            return;
        }
    };
    let root = match job::default_job_root() {
        Ok(root) => root,
        Err(error) => {
            state.status = error;
            state.start_confirm = false;
            return;
        }
    };
    let (acknowledge, acknowledged) = mpsc::channel();
    if device_commands
        .send(WorkerCommand::Suspend(acknowledge))
        .is_err()
        || acknowledged.recv_timeout(Duration::from_secs(10)).is_err()
    {
        let _ = device_commands.send(WorkerCommand::Resume);
        state.status =
            "Device worker did not confirm ownership handoff; Write was not started".into();
        state.start_confirm = false;
        return;
    }
    match job::spawn_detached(spec, &root) {
        Ok(job_state) => {
            state.jobs.insert(0, job_state);
            state.job_index = 0;
            state.page = Page::Jobs;
            state.start_confirm = false;
            state.status = "Detached Write started; closing TUI will not stop it".into();
        }
        Err(error) => {
            let _ = device_commands.send(WorkerCommand::Resume);
            state.status = format!("Failed to start detached Write: {error}");
            state.start_confirm = false;
        }
    }
}

fn open_read_browser(state: &mut UiState, path: &str) {
    let Some(directory) = state
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.volume.as_ref())
        .and_then(|volume| volume.index.as_ref())
        .and_then(|index| index.find_directory(path))
    else {
        state.status = format!("LTFS directory no longer exists: {path}");
        return;
    };
    let mut entries = directory
        .entries
        .iter()
        .map(|entry| match entry {
            crate::ltfs::index::DirectoryEntry::Directory(directory) => TapeBrowserEntry {
                name: directory.name.clone(),
                path: join_ltfs_path(path, &directory.name),
                directory: true,
                size: 0,
            },
            crate::ltfs::index::DirectoryEntry::File(file) => TapeBrowserEntry {
                name: file.name.clone(),
                path: join_ltfs_path(path, &file.name),
                directory: false,
                size: file.length,
            },
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .directory
            .cmp(&left.directory)
            .then_with(|| left.name.cmp(&right.name))
    });
    state.read_tape_directory = path.into();
    state.read_tape_entries = entries;
    state.browser_index = 0;
    state.read_view = ReadView::TapeBrowser;
}

fn join_ltfs_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn toggle_tape_selection(state: &mut UiState, path: String) {
    if let Some(position) = state
        .selected_tape_paths
        .iter()
        .position(|selected| selected == &path)
    {
        state.selected_tape_paths.remove(position);
        state.status = format!("Removed LTFS selection {path}");
    } else {
        state.selected_tape_paths.push(path.clone());
        state.status = format!(
            "Selected {path} ({} roots total)",
            state.selected_tape_paths.len()
        );
    }
}

fn handle_read_key(
    state: &mut UiState,
    code: KeyCode,
    file_commands: &Sender<FileCommand>,
    device_commands: &Sender<WorkerCommand>,
) {
    if state.file_busy {
        return;
    }
    match code {
        KeyCode::Up | KeyCode::Char('k') => {
            state.browser_index = state.browser_index.saturating_sub(1)
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let length = match state.read_view {
                ReadView::TapeBrowser => state.read_tape_entries.len(),
                ReadView::Mounts => state.mounts.len(),
                ReadView::Destination => state.browser_entries.len(),
                ReadView::Plan | ReadView::Confirm => 0,
            };
            state.browser_index = (state.browser_index + 1).min(length.saturating_sub(1));
        }
        KeyCode::Enter if state.read_view == ReadView::TapeBrowser => {
            if let Some(entry) = state.read_tape_entries.get(state.browser_index)
                && entry.directory
            {
                let path = entry.path.clone();
                open_read_browser(state, &path);
            }
        }
        KeyCode::Char(' ') if state.read_view == ReadView::TapeBrowser => {
            if let Some(entry) = state.read_tape_entries.get(state.browser_index) {
                toggle_tape_selection(state, entry.path.clone());
            }
        }
        KeyCode::Char('a') | KeyCode::Char('A') if state.read_view == ReadView::TapeBrowser => {
            toggle_tape_selection(state, state.read_tape_directory.clone());
        }
        KeyCode::Char('s') | KeyCode::Char('S') if state.read_view == ReadView::TapeBrowser => {
            let Some(index) = state
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.volume.as_ref())
                .and_then(|volume| volume.index.as_ref())
            else {
                state.status = "Trusted LTFS index is unavailable".into();
                return;
            };
            match app::plan_ltfs_read(index, &state.selected_tape_paths) {
                Ok(plan) => {
                    state.status = format!(
                        "Read Plan frozen: {} files, {}",
                        plan.files.len(),
                        human_bytes(plan.payload_bytes)
                    );
                    state.read_plan = Some(plan);
                    state.read_view = ReadView::Plan;
                }
                Err(error) => state.status = error,
            }
        }
        KeyCode::Backspace | KeyCode::Esc if state.read_view == ReadView::TapeBrowser => {
            if state.read_tape_directory == "/" {
                state.page = Page::Ltfs;
            } else {
                let parent = state
                    .read_tape_directory
                    .rsplit_once('/')
                    .map_or(
                        "/",
                        |(parent, _)| if parent.is_empty() { "/" } else { parent },
                    )
                    .to_string();
                open_read_browser(state, &parent);
            }
        }
        KeyCode::Enter if state.read_view == ReadView::Plan => {
            state.file_busy = true;
            let _ = file_commands.send(FileCommand::ListMounts);
        }
        KeyCode::Esc if state.read_view == ReadView::Plan => {
            state.read_view = ReadView::TapeBrowser;
            state.read_plan = None;
        }
        KeyCode::Enter if state.read_view == ReadView::Mounts => {
            if let Some(mount) = state.mounts.get(state.browser_index) {
                state.file_busy = true;
                let _ = file_commands.send(FileCommand::Browse(mount.mount_point.clone()));
            }
        }
        KeyCode::Enter if state.read_view == ReadView::Destination => {
            if let Some(entry) = state.browser_entries.get(state.browser_index)
                && entry.kind == app::BrowserEntryKind::Directory
            {
                state.file_busy = true;
                let _ = file_commands.send(FileCommand::Browse(entry.path.clone()));
            }
        }
        KeyCode::Char('s') | KeyCode::Char('S') if state.read_view == ReadView::Destination => {
            let Some(path) = state.browser_path.clone() else {
                return;
            };
            state.read_destination = Some(path);
            state.start_confirm = false;
            state.read_view = ReadView::Confirm;
            state.status = "Review the complete Read Plan before starting".into();
        }
        KeyCode::Backspace | KeyCode::Esc if state.read_view == ReadView::Destination => {
            let Some(current) = state.browser_path.as_ref() else {
                return;
            };
            if state
                .mounts
                .iter()
                .any(|mount| mount.mount_point == *current)
            {
                state.read_view = ReadView::Mounts;
                state.browser_index = 0;
            } else if let Some(parent) = current.parent() {
                state.file_busy = true;
                let _ = file_commands.send(FileCommand::Browse(parent.to_path_buf()));
            }
        }
        KeyCode::Esc if state.read_view == ReadView::Mounts => state.read_view = ReadView::Plan,
        KeyCode::Enter if state.read_view == ReadView::Confirm => state.start_confirm = true,
        KeyCode::Char('y') | KeyCode::Char('Y')
            if state.read_view == ReadView::Confirm && state.start_confirm =>
        {
            start_read_job(state, device_commands);
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc
            if state.read_view == ReadView::Confirm && state.start_confirm =>
        {
            state.start_confirm = false;
        }
        KeyCode::Esc if state.read_view == ReadView::Confirm => {
            state.read_view = ReadView::Destination;
            state.read_destination = None;
        }
        KeyCode::Char('q') | KeyCode::Char('Q') => back_read_level(state),
        _ => {}
    }
}

fn back_read_level(state: &mut UiState) {
    state.start_confirm = false;
    match state.read_view {
        ReadView::TapeBrowser => state.page = Page::Ltfs,
        ReadView::Plan => {
            state.read_view = ReadView::TapeBrowser;
            state.read_plan = None;
        }
        ReadView::Mounts => state.read_view = ReadView::Plan,
        ReadView::Destination => {
            state.read_view = ReadView::Mounts;
            state.browser_index = 0;
        }
        ReadView::Confirm => {
            state.read_view = ReadView::Destination;
            state.read_destination = None;
        }
    }
}

fn start_read_job(state: &mut UiState, device_commands: &Sender<WorkerCommand>) {
    let (Some(snapshot), Some(plan), Some(destination_path)) = (
        state.snapshot.as_ref(),
        state.read_plan.as_ref(),
        state.read_destination.as_ref(),
    ) else {
        state.status = "Read Plan or destination is unavailable".into();
        return;
    };
    if let Err(error) = app::validate_read_plan_destination(plan, destination_path) {
        state.status = error;
        state.start_confirm = false;
        return;
    }
    let mount = state
        .mounts
        .iter()
        .filter(|mount| destination_path.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.components().count());
    let source = job::Endpoint {
        path: plan
            .selections
            .first()
            .cloned()
            .unwrap_or_else(|| "/".into()),
        filesystem_type: None,
        mount_source: None,
    };
    let destination = job::Endpoint {
        path: destination_path.display().to_string(),
        filesystem_type: mount.map(|mount| mount.filesystem_type.clone()),
        mount_source: mount.map(|mount| mount.source.clone()),
    };
    let spec = match job::JobSpec::new(
        job::OperationKind::Read,
        snapshot.drive.sg_path.display().to_string(),
        snapshot.drive.serial.clone(),
        source,
        destination,
        false,
    )
    .with_read_preflight(plan)
    {
        Ok(spec) => spec,
        Err(error) => {
            state.status = error;
            state.start_confirm = false;
            return;
        }
    };
    let root = match job::default_job_root() {
        Ok(root) => root,
        Err(error) => {
            state.status = error;
            return;
        }
    };
    let (acknowledge, acknowledged) = mpsc::channel();
    if device_commands
        .send(WorkerCommand::Suspend(acknowledge))
        .is_err()
        || acknowledged.recv_timeout(Duration::from_secs(10)).is_err()
    {
        let _ = device_commands.send(WorkerCommand::Resume);
        state.status =
            "Device worker did not confirm ownership handoff; Read was not started".into();
        state.start_confirm = false;
        return;
    }
    match job::spawn_detached(spec, &root) {
        Ok(job_state) => {
            state.jobs.insert(0, job_state);
            state.job_index = 0;
            state.page = Page::Jobs;
            state.start_confirm = false;
            state.status = "Detached Read started; closing TUI will not stop it".into();
        }
        Err(error) => {
            let _ = device_commands.send(WorkerCommand::Resume);
            state.status = format!("Failed to start detached Read: {error}");
            state.start_confirm = false;
        }
    }
}

fn handle_job_key(state: &mut UiState, code: KeyCode, commands: &Sender<JobCommand>) {
    if state.cancel_confirm {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(job) = state.jobs.get(state.job_index) {
                    let _ = commands.send(JobCommand::Cancel(job.spec.id.clone()));
                    state.status = "Cancellation requested; waiting for a safe stop point".into();
                }
                state.cancel_confirm = false;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => state.cancel_confirm = false,
            _ => {}
        }
        return;
    }
    match code {
        KeyCode::Up | KeyCode::Char('k') => state.job_index = state.job_index.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            state.job_index = (state.job_index + 1).min(state.jobs.len().saturating_sub(1))
        }
        KeyCode::Char('c') | KeyCode::Char('C')
            if state
                .jobs
                .get(state.job_index)
                .is_some_and(|job| job.phase.is_active()) =>
        {
            state.cancel_confirm = true;
        }
        KeyCode::Enter
            if state
                .jobs
                .get(state.job_index)
                .is_some_and(|job| job.phase.is_terminal() && job.spec.operation.is_write()) =>
        {
            state.page = Page::JobCompletion;
        }
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
            state.page = Page::Overview;
        }
        _ => {}
    }
}

fn render(frame: &mut ratatui::Frame<'_>, state: &UiState) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(format!(
                "Terminal too small\n\nMinimum: {MIN_WIDTH} × {MIN_HEIGHT}\nCurrent: {} × {}",
                area.width, area.height
            ))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(" tapecpy ")),
            area,
        );
        return;
    }
    if state.page == Page::Jobs {
        let layout = Layout::vertical([Constraint::Length(4), Constraint::Min(10)]).split(area);
        render_header(frame, layout[0], state);
        render_jobs(frame, layout[1], state);
        if state.cancel_confirm {
            render_cancel_confirmation(frame, area, state);
        }
        return;
    }
    if state.page == Page::JobCompletion {
        render_job_completion(frame, area, state);
        return;
    }
    if state.page == Page::WriteSource {
        render_write_source(frame, area, state);
        return;
    }
    if state.page == Page::ReadRestore {
        render_read_restore(frame, area, state);
        return;
    }
    if state.page == Page::Format {
        render_format(frame, area, state);
        return;
    }
    if state.page == Page::Erase {
        render_erase(frame, area, state);
        return;
    }
    if !state.selected {
        render_drive_selection(frame, area, state);
        return;
    }

    let layout = Layout::vertical([Constraint::Length(4), Constraint::Min(10)]).split(area);
    render_header(frame, layout[0], state);
    match state.page {
        Page::Overview => render_overview(frame, layout[1], state),
        Page::Ltfs => render_ltfs(frame, layout[1], state),
        Page::Sequential => render_sequential(frame, layout[1], state),
        Page::Health => render_health(frame, layout[1], state),
        Page::Jobs | Page::JobCompletion => unreachable!(),
        Page::ErrorDetails => render_error(frame, layout[1], state),
        Page::WriteSource | Page::ReadRestore | Page::Format | Page::Erase => unreachable!(),
    }
    if let Some(message) = state.busy {
        render_busy(frame, area, message);
    }
}

fn render_drive_selection(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let rows: Vec<Row> = state
        .drives
        .iter()
        .enumerate()
        .map(|(index, drive)| {
            let style = if index == state.drive_index {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(format!("{} {}", drive.vendor, drive.model)),
                Cell::from(drive.serial.clone()),
                Cell::from(drive.nst_path.display().to_string()),
                Cell::from(drive.sg_path.display().to_string()),
            ])
            .style(style)
        })
        .collect();
    let outer = Layout::vertical([Constraint::Min(5), Constraint::Length(2)]).split(area);
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(40),
                Constraint::Percentage(25),
                Constraint::Percentage(18),
                Constraint::Percentage(17),
            ],
        )
        .header(
            Row::new(["Tape Drive", "Serial", "Tape", "SCSI"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Tape Drives "),
        ),
        outer[0],
    );
    frame.render_widget(
        Paragraph::new(format!(
            "↑↓ / j k Select    Enter Open    F4 Jobs ({})    R Rescan    Q Quit",
            state
                .jobs
                .iter()
                .filter(|job| job.phase.is_active())
                .count()
        )),
        outer[1],
    );
}

fn render_format(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(4),
    ])
    .split(area);
    let density = state
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.media.as_ref())
        .and_then(|media| media.density_name())
        .unwrap_or("Unknown LTO media");
    let media_id = state.format_media_id.as_deref().unwrap_or("??");
    let barcode = format!("{}{}", state.format_volume_serial, media_id);
    frame.render_widget(
        Paragraph::new("LTFS Format │ destructive cartridge initialization")
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().borders(Borders::ALL)),
        layout[0],
    );
    let serial_style = if state.format_field == FormatField::VolumeSerial
        && state.format_view == FormatView::Editing
    {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    };
    let name_style = if state.format_field == FormatField::VolumeName
        && state.format_view == FormatView::Editing
    {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    };
    let mut lines = vec![
        line("Cartridge Type", density),
        Line::from(""),
        Line::from(vec![
            Span::styled("Volume Serial       ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("[ {:<6} ]", state.format_volume_serial),
                serial_style,
            ),
            Span::raw("   exactly 6 ASCII letters/digits"),
        ]),
        line(
            "Media ID",
            format!("{media_id} (derived from cartridge density)"),
        ),
        line("Full Barcode", &barcode),
        Line::from(vec![
            Span::styled("Volume Name         ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("[ {} ]", state.format_volume_name), name_style),
        ]),
        Line::from(""),
        line("Compression", "Enabled"),
        line("LTFS block size", "512 KiB"),
        line("Result generation", "1"),
    ];
    match state.format_view {
        FormatView::Confirm => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "FINAL CONFIRMATION: ALL EXISTING DATA AND LTFS METADATA WILL BE DESTROYED",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
        }
        FormatView::Running => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                &state.format_message,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        FormatView::Complete => {
            lines.push(Line::from(""));
            let style = if state.format_result.is_some() {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Red)
            };
            lines.push(Line::from(Span::styled(&state.format_message, style)));
            if let Some(result) = &state.format_result {
                lines.push(line("Volume UUID", &result.volume_uuid));
                lines.push(line("Generation", result.generation));
            }
        }
        FormatView::Editing => {}
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Cartridge / LTFS Identity "),
        ),
        layout[1],
    );
    let help = match state.format_view {
        FormatView::Editing => {
            "Type value  Tab/↑↓ Switch field  Backspace Delete  Enter Review  Q/Esc Back"
        }
        FormatView::Confirm => "Y DESTROY AND FORMAT  N/Esc/Q Return to editing",
        FormatView::Running => {
            "Format in progress — exit and all other device commands are disabled"
        }
        FormatView::Complete => "Q/Esc Return to LTFS Operations",
    };
    frame.render_widget(
        Paragraph::new(format!("{}\n{}", state.format_message, help))
            .block(Block::default().borders(Borders::TOP)),
        layout[2],
    );
}

fn render_erase(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(18),
        Constraint::Length(4),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new("Tape Erase │ destructive media preparation")
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().borders(Borders::ALL)),
        layout[0],
    );

    let snapshot = state.snapshot.as_ref();
    let drive = snapshot.map(|snapshot| &snapshot.drive);
    let media = snapshot.and_then(|snapshot| snapshot.media.as_ref());
    let barcode = media
        .and_then(|media| media.mam.as_ref())
        .and_then(|mam| mam.barcode.as_deref())
        .unwrap_or("—");
    let full_barcode = media
        .and_then(|media| media.full_label_hint())
        .unwrap_or_else(|| barcode.to_owned());
    let volume_name = snapshot
        .and_then(|snapshot| snapshot.volume.as_ref())
        .and_then(|volume| volume.index.as_ref())
        .and_then(|index| index.volume_name())
        .unwrap_or("—");
    let mut lines = vec![
        line(
            "Drive",
            format!(
                "{} {}",
                drive.map_or("—", |drive| drive.vendor.as_str()),
                drive.map_or("—", |drive| drive.model.as_str())
            ),
        ),
        line(
            "Drive Serial",
            drive.map_or("—", |drive| drive.serial.as_str()),
        ),
        line(
            "Cartridge Type",
            media
                .and_then(|media| media.density_name())
                .unwrap_or("Unknown"),
        ),
        line(
            "Current Partition",
            media
                .and_then(|media| media.tape_status)
                .map_or_else(|| "—".into(), |status| status.partition.to_string()),
        ),
        line("MAM Barcode", barcode),
        line("Physical Barcode", full_barcode),
        line("Volume Name", volume_name),
        Line::from(""),
    ];

    for (mode, title, detail) in [
        (
            app::EraseMode::Short,
            "1  Short erase",
            "Fast logical end-of-data reset; old data is not securely overwritten",
        ),
        (
            app::EraseMode::Long,
            "2  Full-tape long erase",
            "Erases the complete unpartitioned tape; may take many hours; not yet hardware-tested",
        ),
        (
            app::EraseMode::MinimumPartitionLong,
            "3  Minimum-partition long erase",
            "Runs a limited mechanical/write check, then restores unpartitioned media (~15 min on tested LTO-5)",
        ),
    ] {
        let selected = mode == state.erase_mode;
        let marker = if selected { ">" } else { " " };
        let style = if selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(format!("{marker} {title}"), style)));
        lines.push(Line::from(format!("    {detail}")));
    }

    match state.erase_view {
        EraseView::Confirm => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "FINAL CONFIRMATION: {} ERASE WILL DESTROY ACCESS TO EXISTING DATA",
                    state.erase_mode.cli_name().to_ascii_uppercase()
                ),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
        }
        EraseView::Running => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                &state.erase_message,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            if let Some(progress) = state.erase_progress {
                lines.push(line(
                    "Device progress",
                    format!("{:.1}%", progress as f64 * 100.0 / u16::MAX as f64),
                ));
            }
        }
        EraseView::Complete => {
            lines.push(Line::from(""));
            let style = if state.erase_result.is_some() {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Red)
            };
            lines.push(Line::from(Span::styled(&state.erase_message, style)));
            if let Some(result) = &state.erase_result {
                lines.push(line("Mode", result.mode.cli_name()));
                lines.push(line(
                    "Elapsed",
                    format!("{} seconds", result.elapsed_seconds),
                ));
                lines.push(line(
                    "LTFS cache",
                    "Cleared; return to Overview and use [6] to probe again",
                ));
            } else {
                lines.push(line(
                    "Recovery",
                    "Inspect Details and media state before Format",
                ));
            }
        }
        EraseView::SelectMode => {}
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Cartridge / Erase Behavior "),
        ),
        layout[1],
    );
    let help = match state.erase_view {
        EraseView::SelectMode => "↑↓/j k or 1/2/3 Select  Enter Review  Q/Esc Back",
        EraseView::Confirm => "Y DESTROY AND ERASE  N/Esc Return to selection",
        EraseView::Running => "Erase in progress — exit and all other device commands are disabled",
        EraseView::Complete => "Q/Esc Return to Overview",
    };
    frame.render_widget(
        Paragraph::new(format!("{}\n{}", state.erase_message, help))
            .block(Block::default().borders(Borders::TOP)),
        layout[2],
    );
}

fn render_write_source(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new("Write workflow │ Select source from an existing Linux mount")
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL)),
        layout[0],
    );
    match state.source_view {
        SourceView::Mounts => {
            let (start, end) = visible_range(
                state.mounts.len(),
                state.browser_index,
                layout[1].height.saturating_sub(3) as usize,
            );
            let rows = state.mounts[start..end]
                .iter()
                .enumerate()
                .map(|(offset, mount)| {
                    let index = start + offset;
                    Row::new([
                        if state.selected_source_roots.contains(&mount.mount_point) {
                            "[x]"
                        } else {
                            "[ ]"
                        },
                        if mount.network { "Network" } else { "Local" },
                        &mount.filesystem_type,
                        mount.mount_point.to_str().unwrap_or("<non-UTF-8>"),
                        &mount.source,
                    ])
                    .style(selection_style(index, state.browser_index))
                });
            frame.render_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(4),
                        Constraint::Length(10),
                        Constraint::Length(12),
                        Constraint::Percentage(38),
                        Constraint::Percentage(42),
                    ],
                )
                .header(
                    Row::new(["Sel", "Class", "Type", "Mount point", "Remote / device"])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Mounted filesystems "),
                ),
                layout[1],
            );
        }
        SourceView::Directory => {
            let (start, end) = visible_range(
                state.browser_entries.len(),
                state.browser_index,
                layout[1].height.saturating_sub(3) as usize,
            );
            let rows =
                state.browser_entries[start..end]
                    .iter()
                    .enumerate()
                    .map(|(offset, entry)| {
                        let index = start + offset;
                        let kind = match entry.kind {
                            app::BrowserEntryKind::Directory => "DIR",
                            app::BrowserEntryKind::File => "FILE",
                            app::BrowserEntryKind::Symlink => "SYMLINK",
                            app::BrowserEntryKind::Other => "OTHER",
                        };
                        Row::new([
                            if state.selected_source_roots.contains(&entry.path) {
                                "[x]".into()
                            } else {
                                "[ ]".into()
                            },
                            kind.to_string(),
                            entry.name.clone(),
                            entry.size.map_or_else(|| "—".into(), human_bytes),
                        ])
                        .style(selection_style(index, state.browser_index))
                    });
            let title = format!(
                " {} ",
                state
                    .browser_path
                    .as_ref()
                    .map_or_else(|| "Directory".into(), |path| path.display().to_string())
            );
            frame.render_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(4),
                        Constraint::Length(10),
                        Constraint::Percentage(70),
                        Constraint::Percentage(30),
                    ],
                )
                .header(
                    Row::new(["Sel", "Kind", "Name", "Size"])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                )
                .block(Block::default().borders(Borders::ALL).title(title)),
                layout[1],
            );
        }
        SourceView::Plan => render_source_plan(frame, layout[1], state),
        SourceView::LtfsDestination => render_tape_destination(frame, layout[1], state),
        SourceView::Confirm => render_write_confirmation(frame, layout[1], state),
    }
    let help = match state.source_view {
        SourceView::Mounts => "↑↓ Select  Enter Browse  Space Toggle  S Scan selection  Q Back",
        SourceView::Directory => {
            "↑↓ Select  Enter Open  Space Toggle  S Scan selection  Esc/Backspace Parent  Q Back"
        }
        SourceView::Plan => "Enter Continue to LTFS destination  Esc Change source  Q Back",
        SourceView::LtfsDestination => {
            "↑↓ Select  Enter Open / select current directory  Esc/Backspace Parent  Q Back"
        }
        SourceView::Confirm if state.start_confirm => {
            "Y Start detached Write  N/Esc Return to review  Q Back"
        }
        SourceView::Confirm if capacity_requires_ack(state) => {
            "A Capacity ack  V Verify  E Auto eject  Enter Continue  Esc Change destination"
        }
        SourceView::Confirm => {
            "V Toggle verify  E Toggle auto eject  Enter Continue  Esc Change destination"
        }
    };
    frame.render_widget(
        Paragraph::new(format!("{}\n{}", state.status, help))
            .block(Block::default().borders(Borders::TOP)),
        layout[2],
    );
    if state.file_busy {
        render_busy(frame, area, "Waiting for filesystem / network mount");
    }
}

fn render_read_restore(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new("Read workflow │ LTFS selection → physical-order plan → host destination")
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL)),
        layout[0],
    );
    match state.read_view {
        ReadView::TapeBrowser => {
            let (start, end) = visible_range(
                state.read_tape_entries.len(),
                state.browser_index,
                layout[1].height.saturating_sub(3) as usize,
            );
            let rows =
                state.read_tape_entries[start..end]
                    .iter()
                    .enumerate()
                    .map(|(offset, entry)| {
                        let index = start + offset;
                        Row::new([
                            if state.selected_tape_paths.contains(&entry.path) {
                                "[x]".into()
                            } else {
                                "[ ]".into()
                            },
                            if entry.directory { "DIR" } else { "FILE" }.into(),
                            entry.name.clone(),
                            if entry.directory {
                                "—".into()
                            } else {
                                human_bytes(entry.size)
                            },
                        ])
                        .style(selection_style(index, state.browser_index))
                    });
            frame.render_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(4),
                        Constraint::Length(8),
                        Constraint::Percentage(70),
                        Constraint::Percentage(30),
                    ],
                )
                .header(
                    Row::new(["Sel", "Kind", "LTFS name", "Logical size"])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" LTFS source: {} ", state.read_tape_directory)),
                ),
                layout[1],
            );
        }
        ReadView::Plan => {
            let plan = state.read_plan.as_ref();
            frame.render_widget(
                Paragraph::new(vec![
                    line("Selected roots", state.selected_tape_paths.len()),
                    line("Directories", plan.map_or(0, |plan| plan.directories.len())),
                    line("Files", plan.map_or(0, |plan| plan.files.len())),
                    line(
                        "Logical payload",
                        plan.map_or_else(|| "—".into(), |plan| human_bytes(plan.payload_bytes)),
                    ),
                    line(
                        "Scheduled extents",
                        plan.map_or(0, |plan| plan.extents.len()),
                    ),
                    line(
                        "Tape order",
                        "partition/start block ascending; output uses file offsets",
                    ),
                    line(
                        "Volume UUID",
                        plan.map_or("—", |plan| plan.volume_uuid.as_str()),
                    ),
                    line(
                        "Index generation",
                        plan.map_or(0, |plan| plan.index_generation),
                    ),
                ])
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Frozen Read Plan "),
                ),
                layout[1],
            );
        }
        ReadView::Mounts => {
            let (start, end) = visible_range(
                state.mounts.len(),
                state.browser_index,
                layout[1].height.saturating_sub(3) as usize,
            );
            let rows = state.mounts[start..end]
                .iter()
                .enumerate()
                .map(|(offset, mount)| {
                    let index = start + offset;
                    Row::new([
                        if mount.network { "Network" } else { "Local" },
                        &mount.filesystem_type,
                        mount.mount_point.to_str().unwrap_or("<non-UTF-8>"),
                        &mount.source,
                    ])
                    .style(selection_style(index, state.browser_index))
                });
            frame.render_widget(
                Table::new(
                    rows,
                    [
                        Constraint::Length(10),
                        Constraint::Length(12),
                        Constraint::Percentage(40),
                        Constraint::Percentage(40),
                    ],
                )
                .header(
                    Row::new(["Class", "Type", "Mount point", "Remote / device"])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Restore destination filesystems "),
                ),
                layout[1],
            );
        }
        ReadView::Destination => {
            let directories = state
                .browser_entries
                .iter()
                .filter(|entry| entry.kind == app::BrowserEntryKind::Directory)
                .collect::<Vec<_>>();
            let (start, end) = visible_range(
                directories.len(),
                state.browser_index,
                layout[1].height.saturating_sub(3) as usize,
            );
            let rows = directories[start..end]
                .iter()
                .enumerate()
                .map(|(offset, entry)| {
                    Row::new(["DIR", entry.name.as_str()])
                        .style(selection_style(start + offset, state.browser_index))
                });
            frame.render_widget(
                Table::new(rows, [Constraint::Length(8), Constraint::Min(20)])
                    .header(
                        Row::new(["Kind", "Directory"])
                            .style(Style::default().add_modifier(Modifier::BOLD)),
                    )
                    .block(Block::default().borders(Borders::ALL).title(format!(
                        " Restore into: {} ",
                        state
                            .browser_path
                            .as_ref()
                            .map_or_else(|| "—".into(), |path| path.display().to_string())
                    ))),
                layout[1],
            );
        }
        ReadView::Confirm => {
            let plan = state.read_plan.as_ref();
            frame.render_widget(
                Paragraph::new(vec![
                    line(
                        "Operation",
                        if state.start_confirm {
                            "Read LTFS — FINAL CONFIRMATION"
                        } else {
                            "Read LTFS"
                        },
                    ),
                    line("Files", plan.map_or(0, |plan| plan.files.len())),
                    line("Directories", plan.map_or(0, |plan| plan.directories.len())),
                    line(
                        "Logical payload",
                        plan.map_or_else(|| "—".into(), |plan| human_bytes(plan.payload_bytes)),
                    ),
                    line(
                        "Destination",
                        state
                            .read_destination
                            .as_ref()
                            .map_or_else(|| "—".into(), |path| path.display().to_string()),
                    ),
                    line("Overwrite", "Never; existing files abort the operation"),
                    line("Execution", "Detached; safe across TUI/SSH disconnect"),
                    line("Tape scheduling", "All extents in physical forward order"),
                ])
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Read Confirmation "),
                ),
                layout[1],
            );
        }
    }
    let help = match state.read_view {
        ReadView::TapeBrowser => {
            "↑↓ Select  Enter Open dir  Space Toggle item  A Toggle current dir  S Build plan  Q Back"
        }
        ReadView::Plan => "Enter Choose restore destination  Esc Change selection  Q Back",
        ReadView::Mounts => "↑↓ Select  Enter Browse mount  Esc Plan  Q Back",
        ReadView::Destination => {
            "↑↓ Select  Enter Open dir  S Select current directory  Esc/Backspace Parent  Q Back"
        }
        ReadView::Confirm if state.start_confirm => {
            "Y Start detached Read  N/Esc Return to review  Q Back"
        }
        ReadView::Confirm => "Enter Final confirmation  Esc Change destination  Q Back",
    };
    frame.render_widget(
        Paragraph::new(format!("{}\n{}", state.status, help))
            .block(Block::default().borders(Borders::TOP)),
        layout[2],
    );
    if state.file_busy {
        render_busy(frame, area, "Waiting for filesystem / network mount");
    }
}

fn render_tape_destination(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let total = state.tape_directories.len() + 1;
    let (start, end) = visible_range(
        total,
        state.browser_index,
        area.height.saturating_sub(3) as usize,
    );
    let rows = (start..end).map(|index| {
        if index == 0 {
            Row::new(["SELECT", ". (this directory)"])
                .style(selection_style(index, state.browser_index))
        } else {
            Row::new(["DIR", state.tape_directories[index - 1].as_str()])
                .style(selection_style(index, state.browser_index))
        }
    });
    frame.render_widget(
        Table::new(rows, [Constraint::Length(12), Constraint::Min(20)])
            .header(
                Row::new(["Action", "LTFS directory"])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" LTFS destination: {} ", state.tape_directory)),
            ),
        area,
    );
}

fn render_write_confirmation(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let plan = state.source_plan.as_ref();
    let capacity = state.capacity.as_ref();
    let warning = match capacity.map(|capacity| capacity.status) {
        Some(app::CapacityStatus::WarningAboveNinetyPercent) => {
            "Selected data exceeds 90% of available LTFS capacity"
        }
        Some(app::CapacityStatus::BlockedInsufficient) => {
            "BLOCKED: selected data exceeds available LTFS capacity"
        }
        Some(app::CapacityStatus::Unknown) => "LTFS available capacity is Unknown",
        _ => "None",
    };
    let lines = vec![
        line(
            "Operation",
            if state.start_confirm {
                "Write LTFS — FINAL CONFIRMATION"
            } else {
                "Write LTFS"
            },
        ),
        line(
            "Source roots",
            plan.map_or_else(
                || "—".into(),
                |plan| format!("{} selected", plan.roots.len()),
            ),
        ),
        line(
            "LTFS destination",
            state.tape_target.as_deref().unwrap_or("—"),
        ),
        line("Files", plan.map_or(0, |plan| plan.files.len())),
        line("Directories", plan.map_or(0, |plan| plan.directories_total)),
        line(
            "Payload",
            plan.map_or_else(|| "—".into(), |plan| human_bytes(plan.payload_bytes)),
        ),
        line(
            "LTFS available",
            capacity
                .and_then(|capacity| capacity.available_bytes)
                .map_or_else(|| "Unknown".into(), human_bytes),
        ),
        line(
            "Planned use",
            capacity
                .and_then(|capacity| capacity.planned_fraction)
                .map_or_else(
                    || "Unknown".into(),
                    |value| format!("{:.1}%", value * 100.0),
                ),
        ),
        line("Capacity warning", warning),
        line(
            "Warning acknowledged",
            if capacity_requires_ack(state) {
                yes_no(state.capacity_acknowledged)
            } else {
                "Not required"
            },
        ),
        line("Read-back verify", yes_no(state.read_back_verify)),
        line(
            "Completion action",
            match state.completion_action {
                CompletionAction::KeepLoaded => "Keep loaded",
                CompletionAction::EjectAfterCommit => "Auto Unload / Eject after commit/verify",
            },
        ),
        Line::from(""),
        Line::from(if state.start_confirm {
            "Press Y to create the detached runner. SSH/TUI exit will not stop the Write."
        } else {
            "No runner exists yet. Enter advances to the final confirmation."
        }),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Operation plan "),
        ),
        area,
    );
}

fn render_source_plan(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let Some(plan) = state.source_plan.as_ref() else {
        return;
    };
    let capacity = state.capacity.as_ref();
    let planned = capacity
        .and_then(|capacity| capacity.planned_fraction)
        .map_or_else(
            || "Unknown".into(),
            |value| format!("{:.1}%", value * 100.0),
        );
    let mut lines = vec![line("Source roots", plan.roots.len())];
    for (index, root) in plan.roots.iter().enumerate().take(5) {
        lines.push(Line::from(format!("  {:<18}{}", index + 1, root.display())));
    }
    if plan.roots.len() > 5 {
        lines.push(line("  …", format!("{} more", plan.roots.len() - 5)));
    }
    lines.extend([
        line("Files", plan.files.len()),
        line("Directories", plan.directories_total),
        line("Payload", human_bytes(plan.payload_bytes)),
        line(
            "LTFS available",
            capacity
                .and_then(|capacity| capacity.available_bytes)
                .map_or_else(|| "Unknown".into(), human_bytes),
        ),
        line("Planned use", planned),
        line(
            "Capacity status",
            capacity.map_or_else(|| "Unknown".into(), |value| format!("{:?}", value.status)),
        ),
        line("Scanned", &plan.scanned_at),
    ]);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Frozen source plan "),
        ),
        area,
    );
}

fn selection_style(index: usize, selected: usize) -> Style {
    if index == selected {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn visible_range(length: usize, selected: usize, capacity: usize) -> (usize, usize) {
    let capacity = capacity.max(1);
    let end = length.min(selected.saturating_add(1).max(capacity));
    (end.saturating_sub(capacity), end)
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn render_jobs(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(9),
        Constraint::Length(11),
        Constraint::Length(7),
        Constraint::Min(6),
        Constraint::Length(2),
    ])
    .split(area);
    let active = state
        .jobs
        .iter()
        .filter(|job| job.phase.is_active())
        .count();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " DETACHED OPERATIONS ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  active {active}  retained {}", state.jobs.len())),
            Span::styled(
                "  |  client detach does not stop runner",
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .block(panel_block(" Jobs ")),
        layout[0],
    );

    let rows = state.jobs.iter().enumerate().map(|(index, job)| {
        let style = if index == state.job_index {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            job_phase_style(job.phase)
        };
        Row::new(vec![
            Cell::from(job.spec.id.as_str().to_string()),
            Cell::from(format!("{:?}", job.spec.operation)),
            Cell::from(format!("{:?}", job.phase)),
            Cell::from(format!(
                "{}/{}",
                job.progress.bytes_completed, job.progress.bytes_total
            )),
            Cell::from(job.updated_at.clone()),
        ])
        .style(style)
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(34),
                Constraint::Length(7),
                Constraint::Length(24),
                Constraint::Percentage(20),
                Constraint::Min(20),
            ],
        )
        .header(Row::new(["Job", "Kind", "Phase", "Bytes", "Updated"]).style(table_header_style()))
        .block(panel_block(" Operations ")),
        layout[1],
    );

    if let Some(job) = state.jobs.get(state.job_index) {
        let percent = if job.progress.bytes_total == 0 {
            "—".into()
        } else {
            format!(
                "{:.1}%",
                job.progress.bytes_completed as f64 * 100.0 / job.progress.bytes_total as f64
            )
        };
        let position = job
            .progress
            .partition
            .zip(job.progress.logical_block)
            .map_or_else(
                || "—".into(),
                |(partition, block)| format!("p{partition}b{block}"),
            );
        let speed = job.progress.tape_bytes_per_second.map_or_else(
            || "—".into(),
            |speed| format!("{:.1} MiB/s", speed / 1024.0 / 1024.0),
        );
        let mut lines = vec![
            line(
                "Source",
                if job.spec.source_roots.is_empty() {
                    job.spec.source.path.clone()
                } else {
                    format!(
                        "{} roots (first: {})",
                        job.spec.source_roots.len(),
                        job.spec.source.path
                    )
                },
            ),
            line("Destination", &job.spec.destination.path),
            line(
                "Drive",
                format!("{} ({})", job.spec.drive_serial, job.spec.drive_selector),
            ),
            line("Progress", percent),
            line(
                "Items",
                format!(
                    "{}/{}",
                    job.progress.items_completed, job.progress.items_total
                ),
            ),
            line("Position", position),
            line("Tape throughput", speed),
            line(
                "Current",
                job.progress.current_item.as_deref().unwrap_or("—"),
            ),
            line("Status", &job.message),
        ];
        if let Some(error) = &job.error {
            lines.push(Line::from(Span::styled(
                format!("Error               {error}"),
                Style::default().fg(Color::Red),
            )));
        }
        if job.requires_diagnosis {
            lines.push(Line::from(Span::styled(
                "Media               consistency diagnosis required",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(panel_block(" Job Snapshot ")),
            layout[2],
        );
        render_job_throughput(frame, layout[3], job);
        render_job_channels(frame, layout[4], job);
    } else {
        frame.render_widget(
            Paragraph::new("No retained Read/Write operations").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Job Snapshot ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            ),
            layout[2],
        );
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("↑↓", shortcut_style()),
            Span::raw(" Select  "),
            Span::styled("Enter", shortcut_style()),
            Span::raw(" Completion  "),
            Span::styled("C", shortcut_style()),
            Span::raw(" Cancel  "),
            Span::styled("Q/Esc", shortcut_style()),
            Span::raw(" Back"),
        ]))
        .style(Style::default().fg(Color::DarkGray)),
        layout[5],
    );
}

fn render_job_completion(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let Some(job) = state.jobs.get(state.job_index) else {
        frame.render_widget(
            Paragraph::new("The selected retained job no longer exists")
                .block(Block::default().borders(Borders::ALL).title(" Completion ")),
            area,
        );
        return;
    };
    let verification = match job.completion.verification {
        VerificationStatus::NotRequested => "Not requested",
        VerificationStatus::Running => "Running",
        VerificationStatus::Passed => "Passed (SHA-256 read-back)",
        VerificationStatus::Failed => "FAILED after index commit",
    };
    let eject = match &job.completion.eject {
        EjectStatus::NotRequested => "Not requested; media remains loaded".into(),
        EjectStatus::Pending => "Pending".into(),
        EjectStatus::Succeeded => "Succeeded".into(),
        EjectStatus::Failed(error) => format!("FAILED: {error}"),
    };
    let mut lines = vec![
        line("Job", job.spec.id.as_str()),
        line("Result", format!("{:?}", job.phase)),
        line("Message", &job.message),
        Line::from(""),
        line(
            "Barcode",
            job.spec.volume_barcode.as_deref().unwrap_or("Unknown"),
        ),
        line(
            "Volume Name",
            job.spec.volume_name.as_deref().unwrap_or("Unknown"),
        ),
        line(
            "LTFS generation",
            job.completion
                .generation
                .map_or_else(|| "Unknown".into(), |value| value.to_string()),
        ),
        line(
            "Index / VCI committed",
            yes_no(job.completion.index_committed),
        ),
        line("Read-back verify", verification),
        line("Unload / Eject", eject),
        line(
            "Error delta W",
            format!(
                "corrected {} / hard {}",
                job.completion
                    .corrected_write_errors
                    .map_or_else(|| "—".into(), |value| value.to_string()),
                job.completion
                    .hard_write_errors
                    .map_or_else(|| "—".into(), |value| value.to_string())
            ),
        ),
        line(
            "Error delta R",
            format!(
                "corrected {} / hard {}",
                job.completion
                    .corrected_read_errors
                    .map_or_else(|| "—".into(), |value| value.to_string()),
                job.completion
                    .hard_read_errors
                    .map_or_else(|| "—".into(), |value| value.to_string())
            ),
        ),
        line(
            "TapeAlert",
            if job.completion.tape_alerts.is_empty() {
                "None".into()
            } else {
                format!("{:?}", job.completion.tape_alerts)
            },
        ),
        line(
            "Session worst",
            job.progress
                .session_worst_channel
                .zip(job.progress.session_worst_channel_rate)
                .map_or_else(
                    || "—".into(),
                    |(channel, rate)| format!("CH{channel:02} {rate:.2}"),
                ),
        ),
        Line::from(""),
        line(
            "Payload",
            format!(
                "{} bytes; {}/{} items",
                job.progress.bytes_completed,
                job.progress.items_completed,
                job.progress.items_total
            ),
        ),
        line(
            "Source",
            if job.spec.source_roots.is_empty() {
                job.spec.source.path.clone()
            } else {
                format!(
                    "{} roots (first: {})",
                    job.spec.source_roots.len(),
                    job.spec.source.path
                )
            },
        ),
        line("Destination", &job.spec.destination.path),
    ];
    if let Some(error) = &job.error {
        lines.push(Line::from(Span::styled(
            format!("Error               {error}"),
            Style::default().fg(Color::Red),
        )));
    }
    if job.requires_diagnosis {
        lines.push(Line::from(Span::styled(
            "Media               consistency diagnosis required before normal write/eject",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    }
    let layout = Layout::vertical([Constraint::Min(10), Constraint::Length(3)]).split(area);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Write Completion — retain Barcode for physical labeling "),
        ),
        layout[0],
    );
    frame.render_widget(
        Paragraph::new("Q/Esc Back to retained jobs").block(Block::default().borders(Borders::TOP)),
        layout[1],
    );
}

fn render_job_throughput(frame: &mut ratatui::Frame<'_>, area: Rect, job: &JobState) {
    let current = job.progress.tape_bytes_per_second.map_or_else(
        || "—".into(),
        |speed| format!("{:.1} MiB/s", speed / 1024.0 / 1024.0),
    );
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(3) as usize;
    let history_start = job
        .progress
        .throughput_history
        .len()
        .saturating_sub(app::PERFORMANCE_DEFAULT_VISIBLE_SAMPLES);
    let samples: Vec<f64> = job
        .progress
        .throughput_history
        .iter()
        .skip(history_start)
        .map(|sample| sample.bytes_per_second)
        .collect();
    let mut lines = braille_area_graph(&samples, inner_width, inner_height);
    let direction = if job.spec.operation.is_read() {
        let buffer = job
            .progress
            .buffer_used_bytes
            .zip(job.progress.buffer_capacity_bytes)
            .map_or_else(
                || "—".into(),
                |(used, capacity)| format!("{} / {}", human_bytes(used), human_bytes(capacity)),
            );
        let pressure = if job.progress.reader_waiting {
            "destination slow / buffer full"
        } else if job.progress.writer_waiting {
            "tape side limiting / buffer empty"
        } else {
            "flowing"
        };
        let filesystem = job
            .spec
            .destination
            .filesystem_type
            .as_deref()
            .unwrap_or("unknown");
        lines.push(Line::from(format!(
            " Buffer {buffer} │ {pressure} │ Destination {filesystem} {}",
            job.spec.destination.path,
        )));
        "Read"
    } else {
        let source = job.progress.source_bytes_per_second.map_or_else(
            || "—".into(),
            |speed| format!("{:.1} MiB/s", speed / 1024.0 / 1024.0),
        );
        let buffer = job
            .progress
            .buffer_used_bytes
            .zip(job.progress.buffer_capacity_bytes)
            .map_or_else(
                || "—".into(),
                |(used, capacity)| format!("{} / {}", human_bytes(used), human_bytes(capacity)),
            );
        let pressure = if job.progress.writer_waiting {
            "source starvation"
        } else if job.progress.reader_waiting {
            "buffer full"
        } else {
            "flowing"
        };
        let filesystem = job
            .spec
            .source
            .filesystem_type
            .as_deref()
            .unwrap_or("unknown");
        lines.push(Line::from(format!(
            " Source I/O {source} │ Buffer {buffer} │ {pressure} │ Source {filesystem}"
        )));
        "Write"
    };
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Tape {direction} Throughput — {current} ")),
        ),
        area,
    );
}

fn render_job_channels(frame: &mut ratatui::Frame<'_>, area: Rect, job: &JobState) {
    let mut lines = Vec::with_capacity(5);
    for row in 0..4 {
        let mut spans = Vec::new();
        for column in 0..4 {
            let channel = row * 4 + column;
            let rate = job
                .progress
                .channel_rates
                .iter()
                .find(|rate| rate.channel == channel);
            let value = rate
                .and_then(|rate| rate.log10_bit_error_rate)
                .map_or_else(|| "   —  ".into(), |rate| format!("{rate:6.2}"));
            let style = if Some(channel) == job.progress.session_worst_channel {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            spans.push(Span::styled(format!(" CH{channel:02} {value}  "), style));
        }
        lines.push(Line::from(spans));
    }
    let sampled = job
        .progress
        .telemetry_updated_at
        .as_deref()
        .unwrap_or("not sampled");
    let worst = job
        .progress
        .session_worst_channel
        .zip(job.progress.session_worst_channel_rate)
        .map_or_else(
            || "—".into(),
            |(channel, rate)| format!("CH{channel:02} {rate:.2}"),
        );
    lines.push(Line::from(format!(
        " Session worst {worst} │ sampled {sampled}"
    )));
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(format!(
            " Channel {} Error Rate — log10(BER) ",
            if job.spec.operation.is_read() {
                "Read"
            } else {
                "Write"
            }
        ))),
        area,
    );
}

fn braille_area_graph(samples: &[f64], width: usize, height: usize) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let pixel_width = width * 2;
    let pixel_height = height * 4;
    let max = samples.iter().copied().fold(0.0_f64, f64::max);
    if samples.is_empty() || max <= 0.0 {
        return (0..height).map(|_| Line::from(" ".repeat(width))).collect();
    }
    let plotted_width = samples.len().min(pixel_width);
    let offset = pixel_width - plotted_width;
    let mut columns = vec![0.0_f64; plotted_width];
    for (sample_index, value) in samples.iter().enumerate() {
        let column = sample_index * plotted_width / samples.len();
        columns[column] = columns[column].max(*value);
    }
    let mut cells = vec![vec![0u8; width]; height];
    for (column, value) in columns.iter().enumerate() {
        let x = offset + column;
        let filled = ((*value / max) * pixel_height as f64)
            .round()
            .clamp(0.0, pixel_height as f64) as usize;
        for from_bottom in 0..filled {
            let y = pixel_height - 1 - from_bottom;
            let cell_x = x / 2;
            let cell_y = y / 4;
            let dot = match (x % 2, y % 4) {
                (0, 0) => 0,
                (0, 1) => 1,
                (0, 2) => 2,
                (0, 3) => 6,
                (1, 0) => 3,
                (1, 1) => 4,
                (1, 2) => 5,
                (1, 3) => 7,
                _ => unreachable!(),
            };
            cells[cell_y][cell_x] |= 1 << dot;
        }
    }
    cells
        .into_iter()
        .map(|row| {
            Line::from(
                row.into_iter()
                    .map(|dots| char::from_u32(0x2800 + dots as u32).unwrap_or(' '))
                    .collect::<String>(),
            )
        })
        .collect()
}

fn job_phase_style(phase: JobPhase) -> Style {
    match phase {
        JobPhase::Completed => Style::default().fg(Color::Green),
        JobPhase::Cancelled => Style::default().fg(Color::Yellow),
        JobPhase::Failed | JobPhase::Interrupted => Style::default().fg(Color::Red),
        JobPhase::CancellationRequested => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::Cyan),
    }
}

fn panel_block(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(74, 88, 110)))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
}

fn table_header_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn shortcut_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn header_status(state: &UiState) -> (&'static str, Style) {
    if state.last_error.is_some() {
        return ("ERROR", Style::default().fg(Color::Red));
    }
    let device_working = state.busy.is_some()
        || state.ltfs_open_pending
        || matches!(state.format_view, FormatView::Running)
        || matches!(state.erase_view, EraseView::Running)
        || selected_device_claimed(state);
    if device_working {
        return ("WORKING", Style::default().fg(Color::Yellow));
    }
    if state.snapshot.as_ref().is_some_and(|snapshot| {
        !snapshot.warnings.is_empty()
            || snapshot
                .diagnosis
                .as_ref()
                .is_some_and(|diagnosis| !diagnosis.safe_for_normal_write)
            || snapshot
                .health
                .as_ref()
                .is_some_and(|health| !health.warnings.is_empty() || !health.tape_alerts.is_empty())
    }) {
        return ("WARNING", Style::default().fg(Color::Yellow));
    }
    match state.snapshot.as_ref().map(|snapshot| snapshot.lifecycle) {
        Some(MediaLifecycle::LoadedThreaded) => ("HEALTHY", Style::default().fg(Color::Green)),
        Some(MediaLifecycle::PresentUnthreaded) => ("READY", Style::default().fg(Color::Cyan)),
        Some(MediaLifecycle::NoMediaDetected) => ("NO MEDIA", Style::default().fg(Color::Gray)),
        _ => ("UNKNOWN", Style::default().fg(Color::DarkGray)),
    }
}

fn render_cancel_confirmation(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let job = state.jobs.get(state.job_index);
    let popup = centered_rect(68, 7, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!(
            "Request cancellation for job {}?\n\nThe runner will stop only at an Application safe boundary.  [Y] Yes  [N] No",
            job.map_or("—", |job| job.spec.id.as_str())
        ))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Confirm Cancellation ")
                .style(Style::default().fg(Color::Yellow)),
        ),
        popup,
    );
}

fn render_header(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let snapshot = state.snapshot.as_ref();
    let drive = snapshot.map(|snapshot| &snapshot.drive);
    let barcode = snapshot
        .and_then(|snapshot| snapshot.media.as_ref())
        .and_then(display_barcode)
        .unwrap_or_else(|| "—".into());
    let medium_serial = snapshot
        .and_then(|snapshot| snapshot.media.as_ref())
        .and_then(|media| media.mam.as_ref())
        .and_then(|mam| mam.medium_serial.as_deref())
        .unwrap_or("—");
    let drive_title = format!(
        "{} {}  ·  {}  ·  {}",
        drive.map_or("—", |drive| drive.vendor.as_str()),
        drive.map_or("—", |drive| drive.model.as_str()),
        drive.map_or("—", |drive| drive.serial.as_str()),
        drive.map_or_else(|| "—".into(), |drive| drive.nst_path.display().to_string()),
    );
    let volume_name = snapshot
        .and_then(|snapshot| snapshot.volume.as_ref())
        .and_then(|volume| volume.index.as_ref())
        .and_then(|index| index.volume_name())
        .unwrap_or("—");
    let media_line = format!(
        "{}  ·  {}  ·  {}  ·  {}",
        lifecycle_label(snapshot.map_or(MediaLifecycle::Unknown, |snapshot| snapshot.lifecycle)),
        barcode,
        volume_name,
        medium_serial,
    );
    let block = panel_block(" tapecpy ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
    let status = header_status(state);
    let temperature = header_temperature(state);
    let top = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(temperature.0.len() as u16 + 2),
        Constraint::Length(status.0.len() as u16 + 2),
    ])
    .split(rows[0]);
    frame.render_widget(
        Paragraph::new(drive_title).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        top[0],
    );
    frame.render_widget(
        Paragraph::new(format!(" {} ", temperature.0))
            .alignment(Alignment::Right)
            .style(temperature.1.add_modifier(Modifier::BOLD)),
        top[1],
    );
    frame.render_widget(
        Paragraph::new(format!(" {} ", status.0))
            .alignment(Alignment::Right)
            .style(status.1.add_modifier(Modifier::BOLD)),
        top[2],
    );
    frame.render_widget(
        Paragraph::new(media_line).style(Style::default().fg(Color::Gray)),
        rows[1],
    );
}

fn header_temperature(state: &UiState) -> (String, Style) {
    let Some(snapshot) = state.snapshot.as_ref() else {
        return ("TEMP —".into(), Style::default().fg(Color::DarkGray));
    };
    let Some(health) = snapshot.health.as_ref() else {
        return ("TEMP ?".into(), Style::default().fg(Color::DarkGray));
    };
    match health
        .temperature
        .and_then(|temperature| temperature.current_celsius)
    {
        Some(value) => (format!("{value}°C"), Style::default().fg(Color::Green)),
        None => ("TEMP N/A".into(), Style::default().fg(Color::Yellow)),
    }
}

fn render_overview(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let Some(snapshot) = state.snapshot.as_ref() else {
        frame.render_widget(Paragraph::new("Waiting for device snapshot…"), area);
        return;
    };
    let rows = Layout::vertical([Constraint::Length(9), Constraint::Min(8)]).split(area);
    let columns =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[0]);
    frame.render_widget(
        Paragraph::new(vec![
            line(
                "Model",
                format!("{} {}", snapshot.drive.vendor, snapshot.drive.model),
            ),
            line("Serial", &snapshot.drive.serial),
            line("Tape device", snapshot.drive.nst_path.display()),
            line("SCSI device", snapshot.drive.sg_path.display()),
        ])
        .block(panel_block(" Drive ")),
        columns[0],
    );
    let media = snapshot.media.as_ref();
    let barcode = media
        .and_then(display_barcode)
        .unwrap_or_else(|| "—".into());
    let write_protect = media
        .and_then(|media| media.tape_status)
        .map(|status| yes_no(status.is_write_protected()))
        .unwrap_or("Unavailable");
    frame.render_widget(
        Paragraph::new(vec![
            line("State", lifecycle_label(snapshot.lifecycle)),
            line("Barcode", barcode),
            line(
                "Media",
                media.and_then(|media| media.density_name()).unwrap_or("—"),
            ),
            line("Write Protect", write_protect),
        ])
        .block(panel_block(" Cartridge ")),
        columns[1],
    );
    render_overview_media(frame, rows[1], state, snapshot);
}

fn display_barcode(media: &crate::device::MediaInfo) -> Option<String> {
    media
        .full_label_hint()
        .or_else(|| media.mam.as_ref()?.barcode.clone())
}

fn render_overview_media(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    state: &UiState,
    snapshot: &DeviceSnapshot,
) {
    let columns =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
    let mam = snapshot.media.as_ref().and_then(|media| media.mam.as_ref());
    frame.render_widget(
        Paragraph::new(vec![
            line(
                "MAM",
                if mam.is_some() {
                    "Available"
                } else {
                    "Unavailable"
                },
            ),
            line(
                "Volume Identifier",
                mam.and_then(|mam| mam.volume_identifier.as_deref())
                    .unwrap_or("—"),
            ),
            line(
                "Manufacturer",
                mam.and_then(|mam| mam.medium_manufacturer.as_deref())
                    .unwrap_or("—"),
            ),
            line(
                "Medium Serial",
                mam.and_then(|mam| mam.medium_serial.as_deref())
                    .unwrap_or("—"),
            ),
            line(
                "Remaining",
                mam_capacity(mam.and_then(|mam| mam.remaining_capacity_mib)),
            ),
            line(
                "Maximum",
                mam_capacity(mam.and_then(|mam| mam.max_capacity_mib)),
            ),
            line("Load Count", counter(mam.and_then(|mam| mam.load_count))),
            line(
                "Total Written",
                mam_capacity(mam.and_then(|mam| mam.total_written_mib)),
            ),
            line(
                "Total Read",
                mam_capacity(mam.and_then(|mam| mam.total_read_mib)),
            ),
        ])
        .block(panel_block(" MAM Cartridge Data ")),
        columns[0],
    );
    let right = Layout::vertical([Constraint::Length(8), Constraint::Min(15)]).split(columns[1]);
    let health = snapshot.health.as_ref();
    frame.render_widget(
        Paragraph::new(vec![
            line(
                "TapeAlert",
                health.map_or_else(
                    || "Unavailable".into(),
                    |health| {
                        if health.tape_alerts.is_empty() {
                            "None".into()
                        } else {
                            format!("{:?}", health.tape_alerts)
                        }
                    },
                ),
            ),
            line(
                "Corrected W",
                counter(
                    health
                        .and_then(|health| health.write_errors.as_ref())
                        .and_then(|value| value.total_corrected),
                ),
            ),
            line(
                "Hard W",
                counter(
                    health
                        .and_then(|health| health.write_errors.as_ref())
                        .and_then(|value| value.uncorrected),
                ),
            ),
            line(
                "Corrected R",
                counter(
                    health
                        .and_then(|health| health.read_errors.as_ref())
                        .and_then(|value| value.total_corrected),
                ),
            ),
            line(
                "Hard R",
                counter(
                    health
                        .and_then(|health| health.read_errors.as_ref())
                        .and_then(|value| value.uncorrected),
                ),
            ),
        ])
        .block(panel_block(" Health (cumulative) ")),
        right[0],
    );
    render_cartridge_operations(frame, right[1], state, snapshot);
}

fn render_cartridge_operations(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    state: &UiState,
    snapshot: &DeviceSnapshot,
) {
    let locked = selected_device_claimed(state);
    let no_media = snapshot.lifecycle == MediaLifecycle::NoMediaDetected;
    let unthreaded = snapshot.lifecycle == MediaLifecycle::PresentUnthreaded;
    let threaded = snapshot.lifecycle == MediaLifecycle::LoadedThreaded;
    let present = unthreaded || threaded;
    let write_protected = snapshot
        .media
        .as_ref()
        .and_then(|media| media.tape_status)
        .is_some_and(|status| status.is_write_protected());
    let operation = |key, action, description, available| {
        cartridge_operation_line(key, action, description, available && !locked, locked)
    };
    frame.render_widget(
        Paragraph::new(vec![
            operation("1", "Load Unthreaded", "Insert only", no_media),
            operation(
                "2",
                "Load & Thread",
                "Insert & thread",
                no_media || unthreaded,
            ),
            operation("3", "Unthread", "Keep inserted", threaded),
            operation("4", "Eject", "Eject directly", present),
            operation("5", "Erase…", "Erase options", threaded && !write_protected),
            operation("6", "LTFS Operations…", "Open workflows", threaded),
            operation("7", "Sequential Operations…", "RAW / TAR", threaded),
            navigation_hint_line("F1", "Overview"),
            navigation_hint_line("F3", "Health"),
            navigation_hint_line("F4", "Jobs"),
            navigation_hint_line("R", "Refresh"),
            navigation_hint_line("Q", "Back / Exit"),
            Line::from(vec![
                Span::styled("Status  ", Style::default().fg(Color::DarkGray)),
                Span::raw(&state.status),
            ]),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Cartridge Operations "),
        ),
        area,
    );
}

fn render_sequential(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let locked = selected_device_claimed(state);
    let writable = state
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.media.as_ref())
        .and_then(|media| media.tape_status)
        .is_none_or(|status| !status.is_write_protected());
    let content = match state.sequential_view {
        SequentialView::Menu => vec![
            ltfs_operation_line("1", "Write RAW image…", writable && !locked),
            ltfs_operation_line("2", "Write TAR archive…", writable && !locked),
            ltfs_operation_line("3", "Recover RAW image…", !locked),
            ltfs_operation_line("4", "Recover TAR image…", !locked),
            Line::from(""),
            Line::from("Operations create detached jobs after path and risk confirmation."),
            Line::from(
                "TAR recovery writes a complete .tar image; tape-side listing is unavailable.",
            ),
            Line::from(""),
            Line::from(vec![
                Span::styled("Status  ", Style::default().fg(Color::DarkGray)),
                Span::raw(&state.status),
            ]),
            Line::from(""),
            navigation_hint_line("Q", "Back to Overview"),
            navigation_hint_line("F4", "Jobs"),
        ],
        SequentialView::Mounts => state
            .mounts
            .iter()
            .enumerate()
            .map(|(index, mount)| {
                Line::from(format!(
                    "{} {:<8} {:<10} {}",
                    if index == state.browser_index {
                        ">"
                    } else {
                        " "
                    },
                    if mount.network { "Network" } else { "Local" },
                    mount.filesystem_type,
                    mount.mount_point.display()
                ))
            })
            .chain(std::iter::once(Line::from("Enter Browse  Q Back")))
            .collect(),
        SequentialView::Directory => state
            .browser_entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                Line::from(format!(
                    "{} {:<8} {}",
                    if index == state.browser_index {
                        ">"
                    } else {
                        " "
                    },
                    format!("{:?}", entry.kind),
                    entry.name
                ))
            })
            .chain(std::iter::once(Line::from(
                if state.sequential_mode.is_some_and(SequentialMode::is_write) {
                    "Enter Open directory  Space Select source  Esc Parent"
                } else {
                    "Enter Open directory  S Use current directory  Esc Parent"
                },
            )))
            .collect(),
        SequentialView::Filename => vec![
            Line::from("Recovery image filename"),
            Line::from(format!("> {}", state.sequential_filename)),
            Line::from("Enter Continue  Esc Back"),
        ],
        SequentialView::Confirm => {
            let mode = state.sequential_mode.expect("mode exists");
            let mut lines = vec![
                Line::from(format!("Operation  {:?}", mode)),
                Line::from(format!(
                    "Path       {}",
                    state
                        .sequential_path
                        .as_ref()
                        .map_or_else(|| "—".into(), |path| path.display().to_string())
                )),
                Line::from(format!(
                    "MAM format  {} / {:?}",
                    state
                        .sequential_mam
                        .as_ref()
                        .and_then(|mam| mam.application_format.as_deref())
                        .unwrap_or("Unknown"),
                    state.sequential_mam.as_ref().map(|mam| mam.status)
                )),
                Line::from(format!(
                    "Destination space  {}",
                    state.sequential_space.as_ref().map_or_else(
                        || "N/A".into(),
                        |space| format!(
                            "{} available / must be > {} ({})",
                            human_bytes(space.available_free_bytes),
                            human_bytes(space.required_free_bytes),
                            if space.sufficient { "Ready" } else { "Blocked" }
                        )
                    )
                )),
            ];
            if mode.is_write() {
                lines.push(Line::from(format!(
                    "MAM overwrite risk acknowledged  {}",
                    if state.sequential_overwrite_ack {
                        "Yes"
                    } else {
                        "No"
                    }
                )));
                lines.push(Line::from(format!(
                    "Read-back SHA-256 verification   {}",
                    if state.read_back_verify { "Yes" } else { "No" }
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(
                    "A Toggle destructive acknowledgement  V Toggle verification",
                ));
            } else {
                lines.push(Line::from(""));
            }
            lines.push(Line::from("Y Start detached job  Q Back"));
            lines
        }
    };
    frame.render_widget(
        Paragraph::new(content).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Sequential RAW / TAR Operations "),
        ),
        area,
    );
    if state.file_busy {
        render_busy(frame, area, "Waiting for filesystem / network mount");
    }
}

fn navigation_hint_line<'a>(key: &'a str, label: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("[{key}] "), Style::default().fg(Color::Cyan)),
        Span::raw(label),
    ])
}

fn display_clock(timestamp: &str) -> &str {
    timestamp
        .split_once('T')
        .and_then(|(_, time)| time.get(..8))
        .unwrap_or(timestamp)
}

fn cartridge_operation_line<'a>(
    key: &'a str,
    action: &'a str,
    description: &'a str,
    available: bool,
    locked: bool,
) -> Line<'a> {
    let style = if available {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let state = if locked {
        "Locked"
    } else if available {
        "Ready"
    } else {
        "—"
    };
    Line::from(vec![
        Span::styled(format!("[{key}] {action:<19}"), style),
        Span::styled(format!("{description:<15}"), style),
        Span::styled(state, style),
    ])
}

fn mam_capacity(value_mib: Option<u64>) -> String {
    value_mib
        .and_then(|value| value.checked_mul(1024 * 1024))
        .map_or_else(|| "—".into(), human_bytes)
}

fn render_ltfs(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let Some(snapshot) = state.snapshot.as_ref() else {
        return;
    };
    if snapshot.lifecycle != MediaLifecycle::LoadedThreaded {
        frame.render_widget(
            Paragraph::new("LTFS unavailable until media is loaded / threaded").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" LTFS Volume "),
            ),
            area,
        );
        return;
    }
    if !state.ltfs_read {
        frame.render_widget(
            Paragraph::new("Reading LTFS partitions, label, index and consistency…").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" LTFS Operations "),
            ),
            area,
        );
        return;
    }
    let volume = snapshot.volume.as_ref();
    let label = volume.and_then(|volume| volume.label.as_ref());
    let index = volume.and_then(|volume| volume.index.as_ref());
    let status = snapshot.media.as_ref().and_then(|media| media.tape_status);
    let diagnosis = snapshot.diagnosis.as_ref();
    let warnings = snapshot
        .warnings
        .iter()
        .map(|warning| ListItem::new(warning.clone()))
        .collect::<Vec<_>>();
    let columns =
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).split(area);
    let left = Layout::vertical([Constraint::Length(15), Constraint::Min(5)]).split(columns[0]);
    frame.render_widget(
        Paragraph::new(vec![
            line(
                "Volume Name",
                index.and_then(|index| index.volume_name()).unwrap_or("—"),
            ),
            line(
                "Generation",
                index.map_or_else(|| "—".into(), |index| index.generation.to_string()),
            ),
            line(
                "Index Partition",
                label.map_or_else(|| "—".into(), |label| label.index_partition.to_string()),
            ),
            line(
                "Data Partition",
                label.map_or_else(|| "—".into(), |label| label.data_partition.to_string()),
            ),
            line(
                "Current Partition",
                status.map_or_else(|| "—".into(), |status| status.partition.to_string()),
            ),
            line(
                "Logical Block",
                status.map_or_else(|| "—".into(), |status| status.block_no.to_string()),
            ),
            line(
                "Index Status",
                if index.is_some() { "OK" } else { "Unavailable" },
            ),
            line(
                "Index / VCI",
                diagnosis.map_or_else(
                    || "Unavailable".into(),
                    |diagnosis| format!("{:?}", diagnosis.consistency),
                ),
            ),
            line(
                "Normal Write",
                diagnosis.map_or("Unknown", |diagnosis| {
                    if diagnosis.safe_for_normal_write {
                        "Allowed"
                    } else {
                        "Blocked"
                    }
                }),
            ),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" LTFS Volume "),
        ),
        left[0],
    );
    frame.render_widget(
        List::new(warnings).block(Block::default().borders(Borders::ALL).title(" Warnings ")),
        left[1],
    );

    let readable = index.is_some();
    let writable = diagnosis.is_some_and(|diagnosis| diagnosis.safe_for_normal_write);
    let format_available = snapshot
        .media
        .as_ref()
        .and_then(|media| media.tape_status)
        .is_none_or(|status| !status.is_write_protected())
        && !selected_device_claimed(state);
    frame.render_widget(
        Paragraph::new(vec![
            ltfs_operation_line("1", "Read LTFS…", readable),
            ltfs_operation_line("2", "Write LTFS…", writable),
            ltfs_operation_line("3", "Format LTFS…", format_available),
            Line::from(""),
            Line::from(vec![
                Span::styled("Status  ", Style::default().fg(Color::DarkGray)),
                Span::raw(&state.status),
            ]),
            Line::from(""),
            navigation_hint_line("F1", "Overview"),
            navigation_hint_line("F3", "Health"),
            navigation_hint_line("F4", "Jobs"),
            navigation_hint_line("Q", "Back / Exit"),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" LTFS Operations "),
        ),
        columns[1],
    );
}

fn ltfs_operation_line<'a>(key: &'a str, action: &'a str, available: bool) -> Line<'a> {
    let style = if available {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Line::from(vec![
        Span::styled(format!("[{key}] {action:<27}"), style),
        Span::styled(if available { "Ready" } else { "—" }, style),
    ])
}

fn render_health(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let split = Layout::vertical([Constraint::Length(11), Constraint::Min(8)]).split(area);
    render_channels(frame, split[0], &state.channels);
    if let Some(snapshot) = state.snapshot.as_ref() {
        let health = snapshot.health.as_ref();
        frame.render_widget(
            Paragraph::new(vec![
                line(
                    "TapeAlert",
                    health.map_or_else(
                        || "Unavailable".into(),
                        |health| {
                            if health.tape_alerts.is_empty() {
                                "None".into()
                            } else {
                                format!("{:?}", health.tape_alerts)
                            }
                        },
                    ),
                ),
                line(
                    "Corrected write",
                    counter(
                        health
                            .and_then(|health| health.write_errors.as_ref())
                            .and_then(|value| value.total_corrected),
                    ),
                ),
                line(
                    "Hard write",
                    counter(
                        health
                            .and_then(|health| health.write_errors.as_ref())
                            .and_then(|value| value.uncorrected),
                    ),
                ),
                line(
                    "Corrected read",
                    counter(
                        health
                            .and_then(|health| health.read_errors.as_ref())
                            .and_then(|value| value.total_corrected),
                    ),
                ),
                line(
                    "Hard read",
                    counter(
                        health
                            .and_then(|health| health.read_errors.as_ref())
                            .and_then(|value| value.uncorrected),
                    ),
                ),
                line("Snapshot", &snapshot.refreshed_at),
            ])
            .block(panel_block(" Drive / Media Health ")),
            split[1],
        );
    }
}

fn render_channels(frame: &mut ratatui::Frame<'_>, area: Rect, channels: &ChannelTelemetryFrame) {
    let mut lines = Vec::new();
    for row in 0..4 {
        let mut spans = Vec::new();
        for column in 0..4 {
            let channel = row * 4 + column;
            let rate = channels.rates.iter().find(|rate| rate.channel == channel);
            let text = match rate.and_then(|rate| rate.log10_bit_error_rate) {
                Some(value) if value.is_infinite() && value.is_sign_negative() => "-inf".into(),
                Some(value) => format!("{value:.2}"),
                None => "—".into(),
            };
            let idle = rate.is_some_and(|rate| !rate.ccp_advanced);
            let worst = channels.worst_now.is_some_and(|worst| {
                rate.and_then(|rate| rate.log10_bit_error_rate)
                    .is_some_and(|value| value.total_cmp(&worst).is_eq())
            });
            let style = if worst {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if idle {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Green)
            };
            spans.push(Span::styled(
                format!(
                    " CH{channel:02} {text:>6}{}   ",
                    if idle { " idle" } else { "     " }
                ),
                style,
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Worst now: {}    Session worst: {}    Updated: {}{}",
        channels
            .worst_now
            .map_or_else(|| "—".into(), |rate| format!("{rate:.2}")),
        channels.session_worst.map_or_else(
            || "—".into(),
            |(channel, rate)| format!("CH{channel:02} {rate:.2}")
        ),
        channels
            .last_success
            .as_deref()
            .unwrap_or("waiting for second sample"),
        if channels.stale { "  [STALE]" } else { "" }
    )));
    if let Some(error) = &channels.last_error {
        lines.push(Line::from(Span::styled(
            error,
            Style::default().fg(Color::Yellow),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel_block(" Channel Error Rate — log10(BER) ")),
        area,
    );
}

fn render_error(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let mut text = state
        .last_error
        .clone()
        .unwrap_or_else(|| "No operation error recorded".into());
    if let Some(snapshot) = &state.snapshot
        && !snapshot.warnings.is_empty()
    {
        text.push_str("\n\nWarnings:\n");
        text.push_str(&snapshot.warnings.join("\n"));
    }
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Error Details "),
        ),
        area,
    );
}

fn render_busy(frame: &mut ratatui::Frame<'_>, area: Rect, message: &str) {
    let popup = centered_rect(58, 7, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!(
            "{message}…\n\nThe tape drive is operating. Do not remove media.\nDevice commands are disabled until completion."
        ))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(" Working ")),
        popup,
    );
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Length(area.height.saturating_sub(height) / 2),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - width_percent) / 2),
        Constraint::Percentage(width_percent),
        Constraint::Min(0),
    ])
    .split(vertical[1])[1]
}

fn line<'a>(label: &'a str, value: impl ToString) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<20}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

fn lifecycle_label(state: MediaLifecycle) -> &'static str {
    match state {
        MediaLifecycle::NoMediaDetected => "No media detected",
        MediaLifecycle::PresentUnthreaded => "Present / Unthreaded",
        MediaLifecycle::LoadedThreaded => "Loaded / Threaded",
        MediaLifecycle::Transitioning => "Transitioning / Not ready",
        MediaLifecycle::Unknown => "Unknown (query failed)",
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "Yes" } else { "No" }
}

fn counter(value: Option<u64>) -> String {
    value.map_or_else(|| "Unavailable".into(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        EraseView, FileCommand, FormatField, FormatView, Page, ReadView, SequentialMode,
        SequentialView, SourceView, UiState, WorkerCommand, WorkerEvent, WorkerOwnershipState,
        apply_worker_event, back_read_level, back_write_level, braille_area_graph,
        cartridge_operation_line, display_clock, handle_erase_key, handle_format_key,
        handle_sequential_key, header_status, ltfs_operation_line,
    };
    use crate::app::EraseMode;
    use crossterm::event::KeyCode;
    use std::sync::mpsc;

    #[test]
    fn suspended_worker_rejects_all_device_access() {
        assert!(WorkerOwnershipState::Active.allows_device_access());
        assert!(!WorkerOwnershipState::Suspending.allows_device_access());
        assert!(!WorkerOwnershipState::Suspended.allows_device_access());
    }

    #[test]
    fn telemetry_timestamp_is_displayed_to_whole_seconds_only() {
        assert_eq!(display_clock("2026-08-14T12:10:35.114344947Z"), "12:10:35");
        assert_eq!(display_clock("unknown"), "unknown");
    }

    #[test]
    fn successful_device_event_clears_current_error_status() {
        let mut state = UiState {
            last_error: Some("temporary device error".into()),
            ..UiState::default()
        };
        assert_eq!(header_status(&state).0, "ERROR");
        apply_worker_event(&mut state, WorkerEvent::Drives(Vec::new()));
        assert!(state.last_error.is_none());
        assert_ne!(header_status(&state).0, "ERROR");
    }

    #[test]
    fn preview_cartridge_operations_are_english_only() {
        let line =
            cartridge_operation_line("3", "Unthread", "Keep inserted", true, false).to_string();
        assert_eq!(line, "[3] Unthread           Keep inserted  Ready");
        assert!(
            !line
                .chars()
                .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character))
        );
    }

    #[test]
    fn ltfs_operations_are_english_only() {
        let line = ltfs_operation_line("1", "Read LTFS…", true).to_string();
        assert_eq!(line, "[1] Read LTFS…                 Ready");
        assert!(
            !line
                .chars()
                .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character))
        );
    }

    #[test]
    fn braille_graph_is_bounded_and_keeps_recent_samples() {
        let graph = braille_area_graph(&[1.0, 2.0, 3.0, 4.0, 5.0], 2, 2);
        assert_eq!(graph.len(), 2);
        assert!(graph.iter().all(|line| line.width() == 2));
        assert_ne!(graph[0].to_string(), "  ");
    }

    #[test]
    fn empty_braille_graph_has_stable_dimensions() {
        let graph = braille_area_graph(&[], 3, 2);
        assert_eq!(graph.len(), 2);
        assert!(graph.iter().all(|line| line.width() == 3));
    }

    #[test]
    fn format_editor_normalizes_serial_and_accepts_unicode_volume_name() {
        let (commands, _receiver) = mpsc::channel();
        let mut state = UiState {
            page: Page::Format,
            format_view: FormatView::Editing,
            format_field: FormatField::VolumeSerial,
            ..UiState::default()
        };
        for character in "ab12cd7".chars() {
            handle_format_key(&mut state, KeyCode::Char(character), &commands);
        }
        assert_eq!(state.format_volume_serial, "AB12CD");
        handle_format_key(&mut state, KeyCode::Tab, &commands);
        for character in "测试卷".chars() {
            handle_format_key(&mut state, KeyCode::Char(character), &commands);
        }
        assert_eq!(state.format_volume_name, "测试卷");
        handle_format_key(&mut state, KeyCode::Enter, &commands);
        assert_eq!(state.format_view, FormatView::Confirm);
    }

    #[test]
    fn confirmed_format_sends_validated_options_and_running_blocks_exit() {
        let (commands, receiver) = mpsc::channel();
        let mut state = UiState {
            page: Page::Format,
            format_view: FormatView::Confirm,
            format_volume_serial: "ABC123".into(),
            format_volume_name: "Archive Test".into(),
            ..UiState::default()
        };
        handle_format_key(&mut state, KeyCode::Char('y'), &commands);
        assert_eq!(state.format_view, FormatView::Running);
        match receiver.try_recv().unwrap() {
            WorkerCommand::Format(options) => {
                assert_eq!(options.barcode, "ABC123");
                assert_eq!(options.volume_name, "Archive Test");
            }
            _ => panic!("expected Format command"),
        }
        handle_format_key(&mut state, KeyCode::Char('q'), &commands);
        assert_eq!(state.page, Page::Format);
        assert_eq!(state.format_view, FormatView::Running);
    }

    #[test]
    fn write_q_navigation_walks_back_one_level_at_a_time() {
        let mut state = UiState {
            page: Page::WriteSource,
            source_view: SourceView::Confirm,
            start_confirm: true,
            tape_target: Some("/restore".into()),
            ..UiState::default()
        };

        back_write_level(&mut state);
        assert_eq!(state.page, Page::WriteSource);
        assert_eq!(state.source_view, SourceView::LtfsDestination);
        assert!(!state.start_confirm);
        assert!(state.tape_target.is_none());

        back_write_level(&mut state);
        assert_eq!(state.source_view, SourceView::Plan);
        back_write_level(&mut state);
        assert_eq!(state.source_view, SourceView::Directory);
        back_write_level(&mut state);
        assert_eq!(state.source_view, SourceView::Mounts);
        back_write_level(&mut state);
        assert_eq!(state.page, Page::Ltfs);
    }

    #[test]
    fn read_q_navigation_walks_back_one_level_at_a_time() {
        let mut state = UiState {
            page: Page::ReadRestore,
            read_view: ReadView::Confirm,
            start_confirm: true,
            read_destination: Some("/restore".into()),
            ..UiState::default()
        };

        back_read_level(&mut state);
        assert_eq!(state.page, Page::ReadRestore);
        assert_eq!(state.read_view, ReadView::Destination);
        assert!(!state.start_confirm);
        assert!(state.read_destination.is_none());

        back_read_level(&mut state);
        assert_eq!(state.read_view, ReadView::Mounts);
        back_read_level(&mut state);
        assert_eq!(state.read_view, ReadView::Plan);
        back_read_level(&mut state);
        assert_eq!(state.read_view, ReadView::TapeBrowser);
        back_read_level(&mut state);
        assert_eq!(state.page, Page::Ltfs);
    }

    #[test]
    fn format_q_navigation_returns_through_format_parent() {
        let (commands, _receiver) = mpsc::channel();
        let mut state = UiState {
            page: Page::Format,
            format_view: FormatView::Confirm,
            ..UiState::default()
        };

        handle_format_key(&mut state, KeyCode::Char('q'), &commands);
        assert_eq!(state.page, Page::Format);
        assert_eq!(state.format_view, FormatView::Editing);
        handle_format_key(&mut state, KeyCode::Char('q'), &commands);
        assert_eq!(state.page, Page::Ltfs);
    }

    #[test]
    fn sequential_menu_reuses_mount_browser_and_returns_one_level() {
        let (file_commands, file_receiver) = mpsc::channel();
        let (device_commands, _device_receiver) = mpsc::channel();
        let mut state = UiState {
            page: Page::Sequential,
            sequential_view: SequentialView::Menu,
            ..UiState::default()
        };

        handle_sequential_key(
            &mut state,
            KeyCode::Char('2'),
            &file_commands,
            &device_commands,
        );
        assert_eq!(state.sequential_mode, Some(SequentialMode::TarWrite));
        assert!(state.file_busy);
        assert!(matches!(
            file_receiver.try_recv(),
            Ok(FileCommand::ListMounts)
        ));

        state.file_busy = false;
        state.sequential_view = SequentialView::Mounts;
        handle_sequential_key(
            &mut state,
            KeyCode::Char('q'),
            &file_commands,
            &device_commands,
        );
        assert_eq!(state.page, Page::Sequential);
        assert_eq!(state.sequential_view, SequentialView::Menu);
        handle_sequential_key(
            &mut state,
            KeyCode::Char('q'),
            &file_commands,
            &device_commands,
        );
        assert_eq!(state.page, Page::Overview);
    }

    #[test]
    fn erase_selector_exposes_all_modes_and_requires_confirmation() {
        let (commands, _receiver) = mpsc::channel();
        let mut state = UiState {
            page: Page::Erase,
            erase_view: EraseView::SelectMode,
            erase_mode: EraseMode::Short,
            ..UiState::default()
        };
        handle_erase_key(&mut state, KeyCode::Char('2'), &commands);
        assert_eq!(state.erase_mode, EraseMode::Long);
        handle_erase_key(&mut state, KeyCode::Char('3'), &commands);
        assert_eq!(state.erase_mode, EraseMode::MinimumPartitionLong);
        handle_erase_key(&mut state, KeyCode::Enter, &commands);
        assert_eq!(state.erase_view, EraseView::Confirm);
    }

    #[test]
    fn confirmed_erase_sends_mode_and_running_blocks_exit() {
        let (commands, receiver) = mpsc::channel();
        let mut state = UiState {
            page: Page::Erase,
            erase_view: EraseView::Confirm,
            erase_mode: EraseMode::Short,
            ..UiState::default()
        };
        handle_erase_key(&mut state, KeyCode::Char('y'), &commands);
        assert_eq!(state.erase_view, EraseView::Running);
        match receiver.try_recv().unwrap() {
            WorkerCommand::Erase(mode) => assert_eq!(mode, EraseMode::Short),
            _ => panic!("expected Erase command"),
        }
        handle_erase_key(&mut state, KeyCode::Char('q'), &commands);
        assert_eq!(state.page, Page::Erase);
        assert_eq!(state.erase_view, EraseView::Running);
    }
}
