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
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Tabs, Wrap,
};

use crate::app::{
    self, ChannelTelemetryFrame, ChannelTelemetryTracker, DeviceSnapshot, MediaLifecycle,
};
use crate::device::TapeDrive;
use crate::job::{self, JobPhase, JobState};

const MIN_WIDTH: u16 = 100;
const MIN_HEIGHT: u16 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Ltfs,
    Health,
    Jobs,
    ErrorDetails,
    WriteSource,
}

enum FileCommand {
    ListMounts,
    Browse(PathBuf),
    Scan(PathBuf, Option<u64>),
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

enum WorkerCommand {
    Discover,
    Select(usize),
    Refresh,
    ReadLtfs,
    Load,
    Unload,
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
    channels: ChannelTelemetryFrame,
    busy: Option<&'static str>,
    status: String,
    last_error: Option<String>,
    jobs: Vec<JobState>,
    job_index: usize,
    cancel_confirm: bool,
    source_view: SourceView,
    mounts: Vec<app::MountedFilesystem>,
    browser_path: Option<PathBuf>,
    browser_entries: Vec<app::BrowserEntry>,
    browser_index: usize,
    source_plan: Option<app::SourcePlan>,
    capacity: Option<app::CapacityAssessment>,
    file_busy: bool,
    tape_directory: String,
    tape_directories: Vec<String>,
    tape_target: Option<String>,
    capacity_acknowledged: bool,
    start_confirm: bool,
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
            channels: ChannelTelemetryFrame::default(),
            busy: Some("Discovering tape drives"),
            status: "Starting Milestone 12 TUI".into(),
            last_error: None,
            jobs: Vec::new(),
            job_index: 0,
            cancel_confirm: false,
            source_view: SourceView::Mounts,
            mounts: Vec::new(),
            browser_path: None,
            browser_entries: Vec::new(),
            browser_index: 0,
            source_plan: None,
            capacity: None,
            file_busy: false,
            tape_directory: "/".into(),
            tape_directories: Vec::new(),
            tape_target: None,
            capacity_acknowledged: false,
            start_confirm: false,
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
            FileCommand::Scan(path, remaining_capacity_mib) => {
                app::scan_source_roots(std::slice::from_ref(&path)).map(|plan| {
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
    state.file_busy = false;
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
            state.status = "Enter opens a directory; Space selects the highlighted source".into();
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
                    let _ = events.send(WorkerEvent::Busy("Loading / threading media"));
                    with_worker_lease(drive, "load", &events, || match app::load_tape(drive) {
                        Ok(()) => {
                            let _ = events.send(WorkerEvent::Status("LOAD completed".into()));
                            refresh_basic_snapshot(drive, &mut channel_tracker, &events);
                        }
                        Err(error) => {
                            let _ = events.send(WorkerEvent::Error(error.to_string()));
                        }
                    });
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Load");
                }
            }
            Ok(WorkerCommand::Unload) => {
                if ownership.allows_device_access()
                    && let Some(drive) = selected.as_ref()
                {
                    let _ = events.send(WorkerEvent::Busy("Unloading / ejecting media"));
                    with_worker_lease(drive, "unload", &events, || match app::unload_tape(drive) {
                        Ok(()) => {
                            let _ = events.send(WorkerEvent::Status("UNLOAD completed".into()));
                            refresh_basic_snapshot(drive, &mut channel_tracker, &events);
                        }
                        Err(error) => {
                            let _ = events.send(WorkerEvent::Error(error.to_string()));
                        }
                    });
                    last_telemetry = Instant::now();
                } else if !ownership.allows_device_access() {
                    reject_suspended_command(&events, "Unload");
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
) {
    match crate::device::lease::DeviceLease::try_acquire(
        &drive.serial,
        crate::device::lease::LeaseOwner::new("tui-worker", operation),
    ) {
        Ok(_lease) => action(),
        Err(error) => {
            let _ = events.send(WorkerEvent::Error(format!(
                "Device lease unavailable for {operation}: {error}"
            )));
        }
    }
}

fn reject_suspended_command(events: &Sender<WorkerEvent>, command: &str) {
    let _ = events.send(WorkerEvent::Status(format!(
        "{command} rejected: device worker is suspended"
    )));
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
            state.status = format!("{} tape drive(s) discovered", state.drives.len());
        }
        WorkerEvent::Snapshot(snapshot, channels, scope) => {
            state.snapshot = Some(*snapshot);
            state.channels = channels;
            state.selected = true;
            state.busy = None;
            state.ltfs_read = scope == SnapshotScope::Ltfs;
            state.status = match scope {
                SnapshotScope::Basic => {
                    "Basic device state refreshed; LTFS information is blank until I Read LTFS"
                        .into()
                }
                SnapshotScope::Ltfs => "LTFS label, index and consistency read completed".into(),
            };
        }
        WorkerEvent::Telemetry(health, channels, timestamp) => {
            if let Some(snapshot) = state.snapshot.as_mut() {
                snapshot.health = Some(*health);
            }
            state.channels = channels;
            state.status = format!("Telemetry refreshed at {timestamp}");
        }
        WorkerEvent::TelemetryUnavailable(channels, reason, timestamp) => {
            state.channels = channels;
            state.status = format!("Telemetry stale at {timestamp}: {reason}");
        }
        WorkerEvent::Status(message) => {
            state.status = message;
            state.busy = None;
        }
        WorkerEvent::Error(error) => {
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
    if state.page == Page::WriteSource {
        handle_source_key(state, code, file_commands, commands);
        return;
    }
    if state.page == Page::Jobs {
        handle_job_key(state, code, job_commands);
        return;
    }
    if matches!(code, KeyCode::F(4) | KeyCode::Char('4')) {
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
                    state.status = "Device opened; LTFS information has not been read".into();
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
            state.selected = false;
            state.snapshot = None;
            state.page = Page::Overview;
        }
        KeyCode::F(1) | KeyCode::Char('1') => state.page = Page::Overview,
        KeyCode::F(2) | KeyCode::Char('2') => state.page = Page::Ltfs,
        KeyCode::F(3) | KeyCode::Char('3') => state.page = Page::Health,
        KeyCode::Char('w') | KeyCode::Char('W') => {
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
                let _ = file_commands.send(FileCommand::ListMounts);
            } else {
                state.status =
                    "Write requires a loaded, healthy LTFS volume; press I to read LTFS first"
                        .into();
            }
        }
        KeyCode::Char('i') | KeyCode::Char('I') => {
            if selected_device_claimed(state) {
                state.status = "LTFS read blocked: detached operation owns this drive".into();
            } else if state
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.lifecycle == MediaLifecycle::LoadedThreaded)
            {
                let _ = commands.send(WorkerCommand::ReadLtfs);
            } else {
                state.status = "LTFS read requires loaded / threaded media".into();
            }
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
        KeyCode::Char('q') | KeyCode::Char('Q') => state.page = Page::Overview,
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
                start_source_scan(state, commands, entry.path.clone());
            }
        }
        KeyCode::Char(' ') if state.source_view == SourceView::Mounts => {
            if let Some(mount) = state.mounts.get(state.browser_index) {
                start_source_scan(state, commands, mount.mount_point.clone());
            }
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

fn start_source_scan(state: &mut UiState, commands: &Sender<FileCommand>, path: PathBuf) {
    let remaining_capacity_mib = state
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.media.as_ref())
        .and_then(|media| media.mam.as_ref())
        .and_then(|mam| mam.remaining_capacity_mib);
    state.file_busy = true;
    state.status = format!("Scanning source {}", path.display());
    let _ = commands.send(FileCommand::Scan(path, remaining_capacity_mib));
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
    let Some(root) = state
        .source_plan
        .as_ref()
        .and_then(|plan| plan.roots.first())
    else {
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
    match app::plan_write_destination(index, root, &state.tape_directory) {
        Ok(target) => {
            state.tape_target = Some(target);
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
    let Some(source_root) = plan.roots.first() else {
        state.status = "Source root is unavailable".into();
        return;
    };
    let Some(tape_target) = state.tape_target.as_ref() else {
        state.status = "LTFS target is unavailable".into();
        return;
    };
    let mount = state
        .mounts
        .iter()
        .filter(|mount| source_root.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.components().count());
    let source = job::Endpoint {
        path: source_root.display().to_string(),
        filesystem_type: mount.map(|mount| mount.filesystem_type.clone()),
        mount_source: mount.map(|mount| mount.source.clone()),
    };
    let destination = job::Endpoint {
        path: tape_target.clone(),
        filesystem_type: None,
        mount_source: None,
    };
    let spec = match job::JobSpec::new(
        job::OperationKind::Write,
        snapshot.drive.sg_path.display().to_string(),
        snapshot.drive.serial.clone(),
        source,
        destination,
        false,
    )
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
        render_jobs(frame, area, state);
        if state.cancel_confirm {
            render_cancel_confirmation(frame, area, state);
        }
        return;
    }
    if state.page == Page::WriteSource {
        render_write_source(frame, area, state);
        return;
    }
    if !state.selected {
        render_drive_selection(frame, area, state);
        return;
    }

    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(3),
    ])
    .split(area);
    render_header(frame, layout[0], state);
    match state.page {
        Page::Overview => render_overview(frame, layout[1], state),
        Page::Ltfs => render_ltfs(frame, layout[1], state),
        Page::Health => render_health(frame, layout[1], state),
        Page::Jobs => unreachable!(),
        Page::ErrorDetails => render_error(frame, layout[1], state),
        Page::WriteSource => unreachable!(),
    }
    render_status(frame, layout[2], state);
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
                        Constraint::Percentage(38),
                        Constraint::Percentage(42),
                    ],
                )
                .header(
                    Row::new(["Class", "Type", "Mount point", "Remote / device"])
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
                        Constraint::Length(10),
                        Constraint::Percentage(70),
                        Constraint::Percentage(30),
                    ],
                )
                .header(
                    Row::new(["Kind", "Name", "Size"])
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
        SourceView::Mounts => "↑↓ Select  Enter Browse  Space Select whole mount  Q Back",
        SourceView::Directory => {
            "↑↓ Select  Enter Open directory  Space Select source  Esc/Backspace Parent  Q Back"
        }
        SourceView::Plan => "Enter Continue to LTFS destination  Esc Change source  Q Back",
        SourceView::LtfsDestination => {
            "↑↓ Select  Enter Open / select current directory  Esc/Backspace Parent  Q Back"
        }
        SourceView::Confirm if state.start_confirm => {
            "Y Start detached Write  N/Esc Return to review"
        }
        SourceView::Confirm if capacity_requires_ack(state) => {
            "A Toggle capacity acknowledgement  Enter Continue  Esc Change destination  Q Back"
        }
        SourceView::Confirm => "Enter Continue  Esc Change destination  Q Back",
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
            "Source",
            plan.and_then(|plan| plan.roots.first())
                .map_or_else(|| "—".into(), |path| path.display().to_string()),
        ),
        line("LTFS target", state.tape_target.as_deref().unwrap_or("—")),
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
        line("Read-back verify", "Disabled (first TUI slice)"),
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
    let root = plan.roots.first();
    let mount = root.and_then(|root| {
        state
            .mounts
            .iter()
            .filter(|mount| root.starts_with(&mount.mount_point))
            .max_by_key(|mount| mount.mount_point.components().count())
    });
    let planned = capacity
        .and_then(|capacity| capacity.planned_fraction)
        .map_or_else(
            || "Unknown".into(),
            |value| format!("{:.1}%", value * 100.0),
        );
    frame.render_widget(
        Paragraph::new(vec![
            line(
                "Source",
                root.map_or_else(|| "—".into(), |root| root.display().to_string()),
            ),
            line(
                "Filesystem",
                mount.map_or("Unknown", |mount| mount.filesystem_type.as_str()),
            ),
            line(
                "Mount source",
                mount.map_or("Unknown", |mount| mount.source.as_str()),
            ),
            line(
                "Network mount",
                mount.map_or("Unknown", |mount| yes_no(mount.network)),
            ),
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
        ])
        .block(
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
        Paragraph::new(format!(
            "tapecpy Jobs │ {} active │ {} retained │ closing TUI only detaches",
            active,
            state.jobs.len()
        ))
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL)),
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
        .header(
            Row::new(["Job", "Kind", "Phase", "Bytes", "Updated"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().borders(Borders::ALL).title(" Operations ")),
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
            line("Source", &job.spec.source.path),
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
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Job Snapshot "),
            ),
            layout[2],
        );
        render_job_throughput(frame, layout[3], job);
        render_job_channels(frame, layout[4], job);
    } else {
        frame.render_widget(
            Paragraph::new("No retained Read/Write operations").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Job Snapshot "),
            ),
            layout[2],
        );
    }
    frame.render_widget(
        Paragraph::new("↑↓ Select    C Request Cancel    Q/Esc Back    Exiting TUI never cancels"),
        layout[5],
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
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Tape Write Throughput — {current} ")),
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
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Channel Error Rate — log10(BER) "),
        ),
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
        .and_then(|media| media.mam.as_ref())
        .and_then(|mam| mam.barcode.as_deref())
        .unwrap_or("—");
    let volume_name = snapshot
        .and_then(|snapshot| snapshot.volume.as_ref())
        .and_then(|volume| volume.index.as_ref())
        .and_then(|index| index.volume_name())
        .unwrap_or("—");
    let title = format!(
        "tapecpy │ {} {} │ {} │ {} │ {} │ {}",
        drive.map_or("—", |drive| drive.vendor.as_str()),
        drive.map_or("—", |drive| drive.model.as_str()),
        drive.map_or("—", |drive| drive.serial.as_str()),
        drive.map_or_else(|| "—".into(), |drive| drive.nst_path.display().to_string()),
        barcode,
        volume_name
    );
    let selected = match state.page {
        Page::Overview => 0,
        Page::Ltfs => 1,
        Page::Health => 2,
        Page::Jobs => 3,
        Page::ErrorDetails => 4,
        Page::WriteSource => 1,
    };
    frame.render_widget(
        Tabs::new([
            title,
            "F2 LTFS".into(),
            "F3 Health".into(),
            "F4 Jobs".into(),
            "D Details".into(),
        ])
        .select(selected)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
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
        .block(Block::default().borders(Borders::ALL).title(" Drive ")),
        columns[0],
    );
    let media = snapshot.media.as_ref();
    let mam = media.and_then(|media| media.mam.as_ref());
    let write_protect = media
        .and_then(|media| media.tape_status)
        .map(|status| yes_no(status.is_write_protected()))
        .unwrap_or("Unavailable");
    frame.render_widget(
        Paragraph::new(vec![
            line("State", lifecycle_label(snapshot.lifecycle)),
            line(
                "Barcode",
                mam.and_then(|mam| mam.barcode.as_deref()).unwrap_or("—"),
            ),
            line(
                "Generation",
                media.and_then(|media| media.density_name()).unwrap_or("—"),
            ),
            line("Write Protect", write_protect),
        ])
        .block(Block::default().borders(Borders::ALL).title(" Cartridge ")),
        columns[1],
    );
    match snapshot.lifecycle {
        MediaLifecycle::NoMediaDetected => frame.render_widget(
            Paragraph::new("Media state: No media detected")
                .block(Block::default().borders(Borders::ALL).title(" Media ")),
            rows[1],
        ),
        MediaLifecycle::PresentUnthreaded => frame.render_widget(
            Paragraph::new("LTFS unavailable until media is threaded\n\n[L] Full Load / Thread    [U] Unload / Eject")
                .block(Block::default().borders(Borders::ALL).title(" LTFS ")),
            rows[1],
        ),
        _ => render_overview_loaded(frame, rows[1], snapshot),
    }
}

fn render_overview_loaded(frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &DeviceSnapshot) {
    let columns =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
    let index = snapshot
        .volume
        .as_ref()
        .and_then(|volume| volume.index.as_ref());
    let diagnosis = snapshot.diagnosis.as_ref();
    frame.render_widget(
        Paragraph::new(vec![
            line(
                "Volume",
                index.and_then(|index| index.volume_name()).unwrap_or("—"),
            ),
            line(
                "Generation",
                index.map_or_else(|| "—".into(), |index| index.generation.to_string()),
            ),
            line("Index", if index.is_some() { "OK" } else { "Unavailable" }),
            line(
                "Consistency",
                diagnosis.map_or_else(
                    || "Unavailable".into(),
                    |diagnosis| format!("{:?}", diagnosis.consistency),
                ),
            ),
            line(
                "Safe to write",
                diagnosis.map_or("Unknown", |diagnosis| {
                    yes_no(diagnosis.safe_for_normal_write)
                }),
            ),
        ])
        .block(Block::default().borders(Borders::ALL).title(" LTFS ")),
        columns[0],
    );
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Health (cumulative) "),
        ),
        columns[1],
    );
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
            Paragraph::new(
                "LTFS information has not been read.\n\nPress [I] Read LTFS when you want to read the label, index and consistency state.",
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" LTFS Volume — not read "),
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
    let split = Layout::vertical([Constraint::Length(15), Constraint::Min(5)]).split(area);
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
        split[0],
    );
    frame.render_widget(
        List::new(warnings).block(Block::default().borders(Borders::ALL).title(" Warnings ")),
        split[1],
    );
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
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Drive / Media Health "),
            ),
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
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Channel Error Rate — log10(BER) "),
        ),
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

fn render_status(frame: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    frame.render_widget(
        Paragraph::new(format!(
            "{}\nF1 Overview  F2 LTFS  F3 Health  F4 Jobs  I Read LTFS  W Write  R Basic refresh  L Load  U Unload  D Details  Q Back",
            state.status
        ))
        .block(Block::default().borders(Borders::TOP)),
        area,
    );
}

fn render_busy(frame: &mut ratatui::Frame<'_>, area: Rect, message: &str) {
    let popup = centered_rect(50, 5, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!("{message}…\n\nUI remains responsive"))
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
    use super::{WorkerOwnershipState, braille_area_graph};

    #[test]
    fn suspended_worker_rejects_all_device_access() {
        assert!(WorkerOwnershipState::Active.allows_device_access());
        assert!(!WorkerOwnershipState::Suspending.allows_device_access());
        assert!(!WorkerOwnershipState::Suspended.allows_device_access());
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
}
