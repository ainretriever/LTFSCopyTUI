//! 可脱离 Read/Write operation 的持久化状态与本机 IPC。
//!
//! 本模块不负责启动常驻 daemon。TUI 在用户最终确认操作后创建 runner；runner
//! 使用这里的状态文件和 Unix socket，让后续客户端 attach、查询或请求安全取消。

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::app::{CancellationToken, WriteEvent, WritePhase};

pub const PROTOCOL_VERSION: u16 = 1;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(String);

impl JobId {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self(format!("{nanos:032x}-{:08x}", std::process::id()))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 80
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err("无效 job id".into());
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Queued,
    Starting,
    Running,
    Finalizing,
    Verifying,
    Ejecting,
    CancellationRequested,
    Cancelled,
    Completed,
    Failed,
    Interrupted,
}

impl JobPhase {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Starting
                | Self::Running
                | Self::Finalizing
                | Self::Verifying
                | Self::Ejecting
                | Self::CancellationRequested
        )
    }

    pub fn is_terminal(self) -> bool {
        !self.is_active()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionAction {
    #[default]
    KeepLoaded,
    EjectAfterCommit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    #[default]
    NotRequested,
    Running,
    Passed,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "message")]
pub enum EjectStatus {
    #[default]
    NotRequested,
    Pending,
    Succeeded,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub path: String,
    pub filesystem_type: Option<String>,
    pub mount_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSpec {
    pub protocol_version: u16,
    pub id: JobId,
    pub operation: OperationKind,
    pub drive_selector: String,
    pub drive_serial: String,
    pub source: Endpoint,
    #[serde(default)]
    pub source_roots: Vec<Endpoint>,
    pub destination: Endpoint,
    pub read_back_verify: bool,
    #[serde(default)]
    pub completion_action: CompletionAction,
    #[serde(default)]
    pub volume_barcode: Option<String>,
    #[serde(default)]
    pub volume_name: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub write_preflight: Option<WritePreflight>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WritePreflight {
    pub roots: Vec<String>,
    pub files_total: usize,
    pub directories_total: usize,
    pub payload_bytes: u64,
    pub scanned_at: String,
    pub available_bytes: Option<u64>,
    pub planned_fraction: Option<f64>,
    pub capacity_sampled_at: String,
    pub capacity_status: JobCapacityStatus,
    pub warning_acknowledged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobCapacityStatus {
    Normal,
    WarningAboveNinetyPercent,
    BlockedInsufficient,
    Unknown,
}

impl From<crate::app::CapacityStatus> for JobCapacityStatus {
    fn from(value: crate::app::CapacityStatus) -> Self {
        match value {
            crate::app::CapacityStatus::Normal => Self::Normal,
            crate::app::CapacityStatus::WarningAboveNinetyPercent => {
                Self::WarningAboveNinetyPercent
            }
            crate::app::CapacityStatus::BlockedInsufficient => Self::BlockedInsufficient,
            crate::app::CapacityStatus::Unknown => Self::Unknown,
        }
    }
}

impl JobSpec {
    pub fn new(
        operation: OperationKind,
        drive_selector: String,
        drive_serial: String,
        source: Endpoint,
        destination: Endpoint,
        read_back_verify: bool,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            id: JobId::new(),
            operation,
            drive_selector,
            drive_serial,
            source,
            source_roots: Vec::new(),
            destination,
            read_back_verify,
            completion_action: CompletionAction::KeepLoaded,
            volume_barcode: None,
            volume_name: None,
            created_at: timestamp_now(),
            write_preflight: None,
        }
    }

    pub fn with_completion(
        mut self,
        action: CompletionAction,
        barcode: Option<String>,
        volume_name: Option<String>,
    ) -> Self {
        self.completion_action = action;
        self.volume_barcode = barcode;
        self.volume_name = volume_name;
        self
    }

    pub fn with_write_preflight(
        mut self,
        plan: &crate::app::SourcePlan,
        capacity: &crate::app::CapacityAssessment,
        warning_acknowledged: bool,
    ) -> Result<Self, String> {
        if self.operation != OperationKind::Write {
            return Err("Read job 不能携带 write preflight".into());
        }
        let configured = self.effective_source_roots();
        let canonical = configured
            .iter()
            .map(|source| {
                std::fs::canonicalize(&source.path)
                    .map_err(|error| format!("无法确认 write source {}: {error}", source.path))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if plan.roots != canonical {
            return Err("source plan 与 job source 不一致".into());
        }
        if capacity.payload_bytes != plan.payload_bytes {
            return Err("capacity assessment 与 source plan 不一致".into());
        }
        self.write_preflight = Some(WritePreflight {
            roots: plan
                .roots
                .iter()
                .map(|root| root.display().to_string())
                .collect(),
            files_total: plan.files.len(),
            directories_total: plan.directories_total,
            payload_bytes: plan.payload_bytes,
            scanned_at: plan.scanned_at.clone(),
            available_bytes: capacity.available_bytes,
            planned_fraction: capacity.planned_fraction,
            capacity_sampled_at: capacity.sampled_at.clone(),
            capacity_status: capacity.status.into(),
            warning_acknowledged,
        });
        self.validate()?;
        Ok(self)
    }

    pub fn with_source_roots(mut self, source_roots: Vec<Endpoint>) -> Self {
        self.source_roots = source_roots;
        self
    }

    fn effective_source_roots(&self) -> Vec<&Endpoint> {
        if self.source_roots.is_empty() {
            vec![&self.source]
        } else {
            self.source_roots.iter().collect()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(format!(
                "不支持 job protocol version {}（当前为 {PROTOCOL_VERSION}）",
                self.protocol_version
            ));
        }
        JobId::parse(self.id.as_str())?;
        if self.drive_selector.is_empty() || self.drive_serial.is_empty() {
            return Err("job 缺少明确的磁带机身份".into());
        }
        if self.source.path.is_empty() || self.destination.path.is_empty() {
            return Err("job source/destination 不能为空".into());
        }
        if self.operation == OperationKind::Read && self.destination.path == "-" {
            return Err("脱离式 Read 不能使用 stdout，必须指定输出文件或目录".into());
        }
        if self.operation == OperationKind::Read
            && (self.read_back_verify || self.completion_action != CompletionAction::KeepLoaded)
        {
            return Err("Read job 不能使用 Write read-back verify 或提交后 eject 策略".into());
        }
        if let Some(preflight) = &self.write_preflight {
            if self.operation != OperationKind::Write {
                return Err("只有 Write job 可以携带 write preflight".into());
            }
            if preflight.roots.is_empty() {
                return Err("write preflight 至少需要一个 source root".into());
            }
            if preflight.roots.len() != self.effective_source_roots().len() {
                return Err("write preflight 与 job source roots 数量不一致".into());
            }
            if preflight.capacity_status == JobCapacityStatus::BlockedInsufficient {
                return Err("source payload 超过 LTFS available capacity".into());
            }
            if matches!(
                preflight.capacity_status,
                JobCapacityStatus::WarningAboveNinetyPercent | JobCapacityStatus::Unknown
            ) && !preflight.warning_acknowledged
            {
                return Err("capacity warning 尚未由用户确认".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobProgress {
    pub current_item: Option<String>,
    pub items_completed: u64,
    pub items_total: u64,
    pub bytes_completed: u64,
    pub bytes_total: u64,
    pub partition: Option<u8>,
    pub logical_block: Option<u64>,
    pub tape_bytes_per_second: Option<f64>,
    #[serde(default)]
    pub source_bytes_per_second: Option<f64>,
    #[serde(default)]
    pub buffer_used_bytes: Option<u64>,
    #[serde(default)]
    pub buffer_capacity_bytes: Option<u64>,
    #[serde(default)]
    pub reader_waiting: bool,
    #[serde(default)]
    pub writer_waiting: bool,
    #[serde(default)]
    pub performance_updated_at: Option<String>,
    pub worst_channel_rate: Option<f64>,
    #[serde(default)]
    pub channel_rates: Vec<crate::device::channel_error::ChannelRate>,
    #[serde(default)]
    pub session_worst_channel: Option<usize>,
    #[serde(default)]
    pub session_worst_channel_rate: Option<f64>,
    #[serde(default)]
    pub telemetry_updated_at: Option<String>,
    #[serde(default)]
    pub throughput_history: Vec<JobThroughputSample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobThroughputSample {
    pub timestamp: String,
    pub bytes_per_second: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobState {
    pub protocol_version: u16,
    pub revision: u64,
    pub spec: JobSpec,
    pub phase: JobPhase,
    pub runner_pid: Option<u32>,
    pub updated_at: String,
    pub message: String,
    pub progress: JobProgress,
    pub error: Option<String>,
    pub requires_diagnosis: bool,
    #[serde(default)]
    pub completion: JobCompletion,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCompletion {
    pub index_committed: bool,
    pub generation: Option<u64>,
    pub verification: VerificationStatus,
    pub eject: EjectStatus,
    pub corrected_write_errors: Option<u64>,
    pub hard_write_errors: Option<u64>,
    pub corrected_read_errors: Option<u64>,
    pub hard_read_errors: Option<u64>,
    pub tape_alerts: Vec<u16>,
}

impl JobState {
    pub fn queued(spec: JobSpec) -> Result<Self, String> {
        spec.validate()?;
        Ok(Self {
            protocol_version: PROTOCOL_VERSION,
            revision: 1,
            updated_at: spec.created_at.clone(),
            spec,
            phase: JobPhase::Queued,
            runner_pid: None,
            message: "等待 operation runner 启动".into(),
            progress: JobProgress::default(),
            error: None,
            requires_diagnosis: false,
            completion: JobCompletion::default(),
        })
    }

    pub fn apply_write_event(&mut self, event: &WriteEvent, timestamp: String) {
        self.revision += 1;
        self.updated_at = timestamp;
        let next_phase = match event.phase {
            WritePhase::Preparing | WritePhase::WritingData => JobPhase::Running,
            WritePhase::FinalizingDataIndex
            | WritePhase::SyncingIndexPartition
            | WritePhase::UpdatingCoherency => JobPhase::Finalizing,
            WritePhase::Verifying => JobPhase::Verifying,
            WritePhase::Failed if event.failure.as_ref().is_some_and(|value| value.cancelled) => {
                JobPhase::Cancelled
            }
            WritePhase::Failed => JobPhase::Failed,
            // Application 完成表示 index/VCI/可选 verify 已结束；runner 可能还要 eject。
            // 在 runner 明确发布最终完成结果前不能提前释放 TUI 的设备所有权。
            WritePhase::Completed => JobPhase::Finalizing,
        };
        if self.phase != JobPhase::CancellationRequested || next_phase.is_terminal() {
            self.phase = next_phase;
        }
        self.message = format!("{:?}", event.phase);
        if event.phase == WritePhase::Verifying {
            self.completion.index_committed = true;
            self.completion.verification = VerificationStatus::Running;
        } else if event.phase == WritePhase::Completed && self.spec.read_back_verify {
            self.completion.verification = VerificationStatus::Passed;
        }
        self.progress.current_item = event.current_file.clone();
        self.progress.items_completed = event.files_completed as u64;
        self.progress.items_total = event.files_total as u64;
        self.progress.bytes_completed = event.bytes_written;
        self.progress.bytes_total = event.bytes_total;
        self.progress.partition = event.partition;
        self.progress.logical_block = event.logical_block;
        if let Some(sample) = &event.telemetry {
            self.progress.worst_channel_rate = sample.worst_rate;
            self.progress.channel_rates = sample.channel_rates.clone();
            self.progress.telemetry_updated_at = Some(sample.timestamp.clone());
            if let Some(rate) = sample.worst_rate.filter(|rate| *rate < 0.0)
                && self
                    .progress
                    .session_worst_channel_rate
                    .is_none_or(|current| rate > current)
                && let Some(channel) = sample.channel_rates.iter().find_map(|channel| {
                    channel
                        .log10_bit_error_rate
                        .filter(|value| value.total_cmp(&rate).is_eq())
                        .map(|_| channel.channel)
                })
            {
                self.progress.session_worst_channel = Some(channel);
                self.progress.session_worst_channel_rate = Some(rate);
            }
        }
        if let Some(sample) = &event.performance {
            self.progress.tape_bytes_per_second = Some(sample.tape_bytes_per_second);
            self.progress.source_bytes_per_second = Some(sample.source_bytes_per_second);
            self.progress.buffer_used_bytes = Some(sample.buffer_used_bytes);
            self.progress.buffer_capacity_bytes = Some(sample.buffer_capacity_bytes);
            self.progress.reader_waiting = sample.reader_waiting;
            self.progress.writer_waiting = sample.writer_waiting;
            self.progress.performance_updated_at = Some(sample.timestamp.clone());
            self.progress.throughput_history.push(JobThroughputSample {
                timestamp: sample.timestamp.clone(),
                bytes_per_second: sample.tape_bytes_per_second,
            });
            if self.progress.throughput_history.len() > crate::app::PERFORMANCE_HISTORY_CAPACITY {
                let excess = self.progress.throughput_history.len()
                    - crate::app::PERFORMANCE_HISTORY_CAPACITY;
                self.progress.throughput_history.drain(..excess);
            }
        }
        if let Some(failure) = &event.failure {
            self.error = Some(failure.message.clone());
            self.requires_diagnosis = failure.requires_diagnosis;
            if failure.commit_state == crate::app::WriteCommitState::Committed {
                self.completion.index_committed = true;
                if self.spec.read_back_verify {
                    self.completion.verification = VerificationStatus::Failed;
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobPaths {
    pub directory: PathBuf,
    pub spec: PathBuf,
    pub state: PathBuf,
    pub socket: PathBuf,
    pub log: PathBuf,
}

impl JobPaths {
    pub fn new(root: &Path, id: &JobId) -> Self {
        let directory = root.join(id.as_str());
        Self {
            spec: directory.join("spec.json"),
            state: directory.join("state.json"),
            socket: directory.join("control.sock"),
            log: directory.join("runner.log"),
            directory,
        }
    }

    pub fn create(&self) -> Result<(), String> {
        fs::create_dir_all(&self.directory)
            .map_err(|error| format!("创建 job 目录失败: {error}"))?;
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("设置 job 目录权限失败: {error}"))
    }
}

pub fn default_job_root() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("tapecpy/jobs"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| "无法确定 HOME".to_string())?;
    Ok(PathBuf::from(home).join(".local/state/tapecpy/jobs"))
}

pub fn save_spec(paths: &JobPaths, spec: &JobSpec) -> Result<(), String> {
    spec.validate()?;
    paths.create()?;
    atomic_json_write(&paths.spec, spec)
}

pub fn save_state(paths: &JobPaths, state: &JobState) -> Result<(), String> {
    paths.create()?;
    atomic_json_write(&paths.state, state)
}

pub fn load_spec(paths: &JobPaths) -> Result<JobSpec, String> {
    read_json(&paths.spec)
}

pub fn load_state(paths: &JobPaths) -> Result<JobState, String> {
    read_json(&paths.state)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let file =
        File::open(path).map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;
    serde_json::from_reader(file).map_err(|error| format!("解析 {} 失败: {error}", path.display()))
}

fn atomic_json_write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| format!("创建 {} 失败: {error}", temporary.display()))?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|error| format!("序列化 {} 失败: {error}", path.display()))?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("同步 {} 失败: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| format!("提交 {} 失败: {error}", path.display()))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Status {
        protocol_version: u16,
    },
    Watch {
        protocol_version: u16,
        after_revision: u64,
        timeout_ms: u64,
    },
    Cancel {
        protocol_version: u16,
    },
}

impl Request {
    fn version(&self) -> u16 {
        match self {
            Self::Status { protocol_version }
            | Self::Watch {
                protocol_version, ..
            }
            | Self::Cancel { protocol_version } => *protocol_version,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum Response {
    State { state: Box<JobState> },
    CancelAccepted { state: Box<JobState> },
    Error { message: String },
}

struct SharedState {
    state: Mutex<JobState>,
    changed: Condvar,
}

#[derive(Clone)]
pub struct JobControl {
    paths: JobPaths,
    shared: Arc<SharedState>,
    cancellation: CancellationToken,
}

impl JobControl {
    pub fn new(
        paths: JobPaths,
        state: JobState,
        cancellation: CancellationToken,
    ) -> Result<Self, String> {
        save_state(&paths, &state)?;
        Ok(Self {
            paths,
            shared: Arc::new(SharedState {
                state: Mutex::new(state),
                changed: Condvar::new(),
            }),
            cancellation,
        })
    }

    pub fn snapshot(&self) -> JobState {
        self.shared.state.lock().unwrap().clone()
    }

    pub fn update(&self, update: impl FnOnce(&mut JobState)) -> Result<JobState, String> {
        let mut state = self.shared.state.lock().unwrap();
        update(&mut state);
        save_state(&self.paths, &state)?;
        let result = state.clone();
        self.shared.changed.notify_all();
        Ok(result)
    }

    pub fn request_cancel(&self, timestamp: String) -> Result<JobState, String> {
        let state = self.update(|state| {
            if state.phase.is_active() && state.phase != JobPhase::CancellationRequested {
                state.revision += 1;
                state.updated_at = timestamp;
                state.phase = JobPhase::CancellationRequested;
                state.message = "已请求取消，等待 Application 安全停止点".into();
            }
        })?;
        if state.phase == JobPhase::CancellationRequested {
            self.cancellation.request_cancel();
        }
        Ok(state)
    }

    fn watch(&self, after_revision: u64, timeout: Duration) -> JobState {
        let state = self.shared.state.lock().unwrap();
        if state.revision > after_revision || state.phase.is_terminal() {
            return state.clone();
        }
        let (state, _) = self.shared.changed.wait_timeout(state, timeout).unwrap();
        state.clone()
    }
}

pub struct IpcServer {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    socket: PathBuf,
}

impl IpcServer {
    pub fn start(control: JobControl) -> Result<Self, String> {
        let socket = control.paths.socket.clone();
        if socket.exists() {
            fs::remove_file(&socket).map_err(|error| format!("删除旧 IPC socket 失败: {error}"))?;
        }
        let listener = UnixListener::bind(&socket)
            .map_err(|error| format!("创建 IPC socket {} 失败: {error}", socket.display()))?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("设置 IPC socket 权限失败: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("设置 IPC socket nonblocking 失败: {error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("tapecpy-job-ipc".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let connection_control = control.clone();
                            let _ = thread::Builder::new()
                                .name("tapecpy-job-ipc-client".into())
                                .spawn(move || handle_connection(stream, &connection_control));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| format!("启动 IPC thread 失败: {error}"))?;
        Ok(Self {
            stop,
            thread: Some(thread),
            socket,
        })
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.socket);
    }
}

fn handle_connection(mut stream: UnixStream, control: &JobControl) {
    let response = match read_request(&stream) {
        Ok(request) => handle_request(request, control),
        Err(message) => Response::Error { message },
    };
    let _ = serde_json::to_writer(&mut stream, &response);
    let _ = stream.write_all(b"\n");
}

fn handle_request(request: Request, control: &JobControl) -> Response {
    match request {
        request if request.version() != PROTOCOL_VERSION => Response::Error {
            message: format!(
                "protocol version 不匹配：client={} runner={PROTOCOL_VERSION}",
                request.version()
            ),
        },
        Request::Status { .. } => Response::State {
            state: Box::new(control.snapshot()),
        },
        Request::Watch {
            after_revision,
            timeout_ms,
            ..
        } => Response::State {
            state: Box::new(control.watch(
                after_revision,
                Duration::from_millis(timeout_ms.min(30_000)),
            )),
        },
        Request::Cancel { .. } => match control.request_cancel(timestamp_now()) {
            Ok(state) if state.phase == JobPhase::CancellationRequested => {
                Response::CancelAccepted {
                    state: Box::new(state),
                }
            }
            Ok(state) => Response::Error {
                message: format!("job 已处于终态 {:?}，不能取消", state.phase),
            },
            Err(message) => Response::Error { message },
        },
    }
}

fn read_request(stream: &UnixStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream);
    let mut message = Vec::new();
    reader
        .by_ref()
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_until(b'\n', &mut message)
        .map_err(|error| format!("读取 IPC request 失败: {error}"))?;
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("IPC request 超过大小限制".into());
    }
    serde_json::from_slice(&message).map_err(|error| format!("解析 IPC request 失败: {error}"))
}

pub fn request(socket: &Path, request: &Request) -> Result<Response, String> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|error| format!("连接 job IPC {} 失败: {error}", socket.display()))?;
    serde_json::to_writer(&mut stream, request)
        .map_err(|error| format!("序列化 IPC request 失败: {error}"))?;
    stream
        .write_all(b"\n")
        .map_err(|error| format!("发送 IPC request 失败: {error}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| format!("结束 IPC request 失败: {error}"))?;
    let mut reader = BufReader::new(stream);
    let mut message = Vec::new();
    reader
        .by_ref()
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_until(b'\n', &mut message)
        .map_err(|error| format!("读取 IPC response 失败: {error}"))?;
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("IPC response 超过大小限制".into());
    }
    serde_json::from_slice(&message).map_err(|error| format!("解析 IPC response 失败: {error}"))
}

pub fn reconcile_interrupted(
    mut state: JobState,
    process_alive: impl FnOnce(u32) -> bool,
) -> JobState {
    if state.phase.is_active() && state.runner_pid.is_some_and(|pid| !process_alive(pid)) {
        state.revision += 1;
        state.phase = JobPhase::Interrupted;
        state.updated_at = timestamp_now();
        state.message = "runner 已消失；禁止自动续写，必须检查 operation 结果".into();
        state.error = Some("detached operation runner terminated unexpectedly".into());
        state.requires_diagnosis = state.spec.operation == OperationKind::Write;
    }
    state
}

/// 用户在确认页选择 Start 后调用。该函数只创建一个 operation runner，不启动常驻服务。
pub fn spawn_detached(spec: JobSpec, root: &Path) -> Result<JobState, String> {
    spec.validate()?;
    let paths = JobPaths::new(root, &spec.id);
    paths.create()?;
    if paths.state.exists() || paths.spec.exists() {
        return Err(format!("job {} 已存在", spec.id));
    }
    save_spec(&paths, &spec)?;
    let state = JobState::queued(spec)?;
    save_state(&paths, &state)?;

    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&paths.log)
        .map_err(|error| format!("创建 runner log 失败: {error}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|error| format!("复制 runner log handle 失败: {error}"))?;
    let executable =
        std::env::current_exe().map_err(|error| format!("无法确定 tapecpy executable: {error}"))?;
    let mut command = Command::new(executable);
    command
        .arg("_job-runner")
        .arg(root)
        .arg(state.spec.id.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    // SAFETY: pre_exec 中只调用 async-signal-safe 的 setsid；失败会阻止 exec。
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 detached operation runner 失败: {error}"))?;
    // 前台进程存活时负责回收 child；reaper thread 不拥有任何 workflow 状态，前台
    // 退出后 runner 仍由新 session 独立运行。
    let _ = thread::Builder::new()
        .name("tapecpy-job-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(state)
}

/// 隐藏的 runner 入口。只能由 `spawn_detached` 创建的新 session 调用。
pub fn run_detached(root: &Path, id: &JobId) -> Result<(), String> {
    let paths = JobPaths::new(root, id);
    let state = load_state(&paths)?;
    if state.spec.id != *id {
        return Err("job id 与持久化 spec 不一致".into());
    }
    let cancellation = CancellationToken::default();
    let control = JobControl::new(paths.clone(), state, cancellation.clone())?;
    let _ipc = IpcServer::start(control.clone())?;
    control.update(|state| {
        state.revision += 1;
        state.phase = JobPhase::Starting;
        state.runner_pid = Some(std::process::id());
        state.updated_at = timestamp_now();
        state.message = "operation runner 已启动，正在取得设备所有权".into();
    })?;

    let result = run_operation(root, &control, &cancellation);
    if let Err(error) = &result {
        let cancelled = error.contains("[cancelled]") || cancellation.is_cancelled();
        control.update(|state| {
            state.revision += 1;
            state.updated_at = timestamp_now();
            state.phase = if cancelled {
                JobPhase::Cancelled
            } else {
                JobPhase::Failed
            };
            state.message = if cancelled {
                "operation 已在安全边界取消".into()
            } else {
                "operation runner 失败".into()
            };
            state.error = Some(error.replace("[cancelled]", ""));
            if state.spec.operation == OperationKind::Write && state.progress.bytes_completed > 0 {
                state.requires_diagnosis = true;
            }
        })?;
    }
    result
}

fn run_operation(
    _root: &Path,
    control: &JobControl,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let spec = control.snapshot().spec;
    let _lease = crate::device::lease::DeviceLease::try_acquire(
        &spec.drive_serial,
        crate::device::lease::LeaseOwner::job_runner(
            format!("{:?}", spec.operation),
            spec.id.as_str(),
        ),
    )?;
    let drives = crate::app::discover_drives().map_err(|error| error.to_string())?;
    let drive = crate::app::select_drive(&drives, Some(&spec.drive_selector))?;
    if drive.serial != spec.drive_serial {
        return Err(format!(
            "磁带机身份变化：计划 serial={}，当前 serial={}",
            spec.drive_serial, drive.serial
        ));
    }
    match spec.operation {
        OperationKind::Write => {
            for endpoint in spec.effective_source_roots() {
                validate_host_mount(endpoint)?;
            }
        }
        OperationKind::Read => validate_host_mount(&spec.destination)?,
    }
    if let Some(preflight) = spec.write_preflight.as_ref() {
        let media = crate::app::inspect_media(drive).map_err(|error| error.to_string())?;
        let current_capacity = crate::app::assess_write_capacity(
            preflight.payload_bytes,
            media
                .mam
                .as_ref()
                .and_then(|mam| mam.remaining_capacity_mib),
            timestamp_now(),
        );
        match current_capacity.status {
            crate::app::CapacityStatus::BlockedInsufficient => {
                return Err(format!(
                    "runner 启动前容量复核失败：payload={} bytes，available={} bytes",
                    preflight.payload_bytes,
                    current_capacity.available_bytes.unwrap_or(0)
                ));
            }
            crate::app::CapacityStatus::WarningAboveNinetyPercent
            | crate::app::CapacityStatus::Unknown
                if !preflight.warning_acknowledged =>
            {
                return Err("runner 容量复核产生尚未确认的 warning；请返回确认页".into());
            }
            _ => {}
        }
    }
    control.update(|state| {
        state.revision += 1;
        state.phase = JobPhase::Running;
        state.updated_at = timestamp_now();
        state.message = format!("{:?} operation 正在运行", state.spec.operation);
    })?;

    match spec.operation {
        OperationKind::Write => {
            let mut observer = |event: &WriteEvent| {
                let _ = control.update(|state| {
                    state.apply_write_event(event, timestamp_now());
                });
            };
            let options = crate::app::WriteOptions {
                verification: if spec.read_back_verify {
                    crate::app::WriteVerification::ReadBackSha256
                } else {
                    crate::app::WriteVerification::None
                },
                expected_source: spec.write_preflight.as_ref().map(|preflight| {
                    crate::app::WritePlanExpectation {
                        files: preflight.files_total,
                        directories: preflight.directories_total,
                        payload_bytes: preflight.payload_bytes,
                    }
                }),
                failpoint: None,
                cancellation: Some(cancellation.clone()),
                cancelpoint: None,
            };
            let session = crate::app::WriteSession::new(drive);
            let result = if spec.source_roots.is_empty() {
                session.run_detailed_with_options(
                    Path::new(&spec.source.path),
                    &spec.destination.path,
                    options,
                    &mut observer,
                )
            } else {
                let roots = spec
                    .source_roots
                    .iter()
                    .map(|endpoint| PathBuf::from(&endpoint.path))
                    .collect::<Vec<_>>();
                session.run_roots_detailed_with_options(
                    &roots,
                    &spec.destination.path,
                    options,
                    &mut observer,
                )
            }
            .map_err(|error| error.to_string())?;
            control.update(|state| {
                state.revision += 1;
                state.updated_at = timestamp_now();
                state.completion.index_committed = true;
                state.completion.generation = Some(result.generation);
                state.completion.verification = if spec.read_back_verify {
                    VerificationStatus::Passed
                } else {
                    VerificationStatus::NotRequested
                };
                state.completion.corrected_write_errors =
                    result.health_delta.corrected_write_errors;
                state.completion.hard_write_errors = result.health_delta.hard_write_errors;
                state.completion.corrected_read_errors = result.health_delta.corrected_read_errors;
                state.completion.hard_read_errors = result.health_delta.hard_read_errors;
                state.completion.tape_alerts = result.health_delta.active_tape_alerts.clone();
                state.completion.eject = match spec.completion_action {
                    CompletionAction::KeepLoaded => EjectStatus::NotRequested,
                    CompletionAction::EjectAfterCommit => EjectStatus::Pending,
                };
                state.phase = if spec.completion_action == CompletionAction::EjectAfterCommit {
                    JobPhase::Ejecting
                } else {
                    JobPhase::Completed
                };
                state.message = if spec.completion_action == CompletionAction::EjectAfterCommit {
                    "写入和索引提交已完成，正在自动 Eject".into()
                } else {
                    "Write operation 已完成；磁带保持装载".into()
                };
            })?;
            if spec.completion_action == CompletionAction::EjectAfterCommit {
                match crate::app::unload_tape(drive) {
                    Ok(()) => control.update(|state| {
                        state.revision += 1;
                        state.updated_at = timestamp_now();
                        state.phase = JobPhase::Completed;
                        state.completion.eject = EjectStatus::Succeeded;
                        state.message = "Write operation 已完成，磁带已自动弹出".into();
                    })?,
                    Err(error) => control.update(|state| {
                        state.revision += 1;
                        state.updated_at = timestamp_now();
                        state.phase = JobPhase::Completed;
                        state.completion.eject = EjectStatus::Failed(error.to_string());
                        state.message = "写入和索引提交成功，但自动弹出失败".into();
                        state.error = Some(format!("自动 Eject 失败：{error}"));
                    })?,
                };
            }
        }
        OperationKind::Read => {
            let output_path = Path::new(&spec.destination.path);
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(output_path)
                .map_err(|error| {
                    format!(
                        "创建 Read destination {} 失败: {error}",
                        output_path.display()
                    )
                })?;
            let mut last_persist = std::time::Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now);
            let mut observer = |event: &crate::app::ReadEvent| {
                if last_persist.elapsed() < Duration::from_millis(250) {
                    return;
                }
                last_persist = std::time::Instant::now();
                let _ = control.update(|state| {
                    state.revision += 1;
                    state.updated_at = timestamp_now();
                    state.progress.current_item = Some(event.tape_path.clone());
                    state.progress.items_total = 1;
                    state.progress.bytes_completed = event.bytes_read;
                    state.progress.bytes_total = event.bytes_total;
                    state.progress.partition = event.partition;
                    state.progress.logical_block = event.logical_block;
                });
            };
            let bytes_read = crate::app::read_file_with_observer(
                drive,
                &spec.source.path,
                &mut output,
                cancellation,
                &mut observer,
            )?;
            output
                .sync_all()
                .map_err(|error| format!("同步 Read destination 失败: {error}"))?;
            control.update(|state| {
                state.revision += 1;
                state.phase = JobPhase::Completed;
                state.updated_at = timestamp_now();
                state.message = "Read operation 已完成".into();
                state.progress.items_completed = 1;
                state.progress.items_total = 1;
                state.progress.bytes_completed = bytes_read;
                state.progress.bytes_total = bytes_read;
            })?;
        }
    }
    Ok(())
}

fn validate_host_mount(endpoint: &Endpoint) -> Result<(), String> {
    let (Some(expected_type), Some(expected_source)) = (
        endpoint.filesystem_type.as_deref(),
        endpoint.mount_source.as_deref(),
    ) else {
        return Ok(());
    };
    let current = crate::app::mounted_filesystem_for_path(Path::new(&endpoint.path))?
        .ok_or_else(|| format!("host path {} 当前不属于任何 mount", endpoint.path))?;
    if current.filesystem_type != expected_type || current.source != expected_source {
        return Err(format!(
            "host mount identity changed for {}: planned {} {}, current {} {}",
            endpoint.path, expected_type, expected_source, current.filesystem_type, current.source
        ));
    }
    Ok(())
}

pub fn query_state(root: &Path, id: &JobId) -> Result<JobState, String> {
    let paths = JobPaths::new(root, id);
    match request(
        &paths.socket,
        &Request::Status {
            protocol_version: PROTOCOL_VERSION,
        },
    ) {
        Ok(Response::State { state }) => Ok(*state),
        Ok(Response::Error { message }) => Err(message),
        Ok(_) => Err("job runner 返回了意外响应".into()),
        Err(_) => {
            let state = load_state(&paths)?;
            let reconciled = reconcile_interrupted(state.clone(), process_is_alive);
            if reconciled != state {
                save_state(&paths, &reconciled)?;
            }
            Ok(reconciled)
        }
    }
}

pub fn cancel(root: &Path, id: &JobId) -> Result<JobState, String> {
    let paths = JobPaths::new(root, id);
    match request(
        &paths.socket,
        &Request::Cancel {
            protocol_version: PROTOCOL_VERSION,
        },
    )? {
        Response::CancelAccepted { state } => Ok(*state),
        Response::Error { message } => Err(message),
        _ => Err("job runner 返回了意外响应".into()),
    }
}

/// 为 TUI 的“可重新连接任务”列表提供持久化快照；损坏的单个 job 不阻塞其余任务。
pub fn list_states(root: &Path) -> Result<Vec<JobState>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(root)
        .map_err(|error| format!("读取 job 根目录 {} 失败: {error}", root.display()))?;
    let mut states = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(id) = JobId::parse(entry.file_name().to_string_lossy().into_owned()) else {
            continue;
        };
        if let Ok(state) = query_state(root, &id) {
            states.push(state);
        }
    }
    states.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(states)
}

pub fn watch_state(
    root: &Path,
    id: &JobId,
    after_revision: u64,
    timeout: Duration,
) -> Result<JobState, String> {
    let paths = JobPaths::new(root, id);
    match request(
        &paths.socket,
        &Request::Watch {
            protocol_version: PROTOCOL_VERSION,
            after_revision,
            timeout_ms: timeout.as_millis().min(30_000) as u64,
        },
    ) {
        Ok(Response::State { state }) => Ok(*state),
        Ok(Response::Error { message }) => Err(message),
        Ok(_) => Err("job runner 返回了意外响应".into()),
        Err(_) => query_state(root, id),
    }
}

fn process_is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 不发送信号，只检查进程是否存在/可访问。
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub fn timestamp_now() -> String {
    crate::app::ltfs_time_now()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(name: &str) -> JobPaths {
        let root = std::env::temp_dir().join(format!(
            "tapecpy-job-test-{}-{}-{name}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        JobPaths::new(&root, &JobId::parse("test-job").unwrap())
    }

    fn spec(operation: OperationKind) -> JobSpec {
        JobSpec {
            protocol_version: PROTOCOL_VERSION,
            id: JobId::parse("test-job").unwrap(),
            operation,
            drive_selector: "/dev/sg1".into(),
            drive_serial: "TEST123".into(),
            source: Endpoint {
                path: "/source".into(),
                filesystem_type: Some("nfs4".into()),
                mount_source: Some("nas:/archive".into()),
            },
            source_roots: Vec::new(),
            destination: Endpoint {
                path: "/destination".into(),
                filesystem_type: None,
                mount_source: None,
            },
            read_back_verify: false,
            completion_action: CompletionAction::KeepLoaded,
            volume_barcode: None,
            volume_name: None,
            created_at: "2026-08-10T00:00:00Z".into(),
            write_preflight: None,
        }
    }

    #[test]
    fn detached_read_rejects_stdout_destination() {
        let mut value = spec(OperationKind::Read);
        value.destination.path = "-".into();
        assert!(value.validate().unwrap_err().contains("stdout"));
    }

    #[test]
    fn detached_read_rejects_write_completion_options() {
        let mut value = spec(OperationKind::Read);
        value.completion_action = CompletionAction::EjectAfterCommit;
        assert!(value.validate().unwrap_err().contains("Read job"));
        value.completion_action = CompletionAction::KeepLoaded;
        value.read_back_verify = true;
        assert!(value.validate().unwrap_err().contains("Read job"));
    }

    #[test]
    fn write_preflight_requires_capacity_acknowledgement_and_matching_source() {
        let root = std::env::temp_dir().join(format!(
            "tapecpy-preflight-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("one.bin"), vec![0_u8; 950_000]).unwrap();
        let plan = crate::app::scan_source_roots(std::slice::from_ref(&root)).unwrap();
        let warning = crate::app::assess_write_capacity(plan.payload_bytes, Some(1), "sampled");

        let mut value = spec(OperationKind::Write);
        value.source.path = root.display().to_string();
        assert!(
            value
                .clone()
                .with_write_preflight(&plan, &warning, false)
                .unwrap_err()
                .contains("warning")
        );
        assert!(value.with_write_preflight(&plan, &warning, true).is_ok());

        let mut wrong_source = spec(OperationKind::Write);
        wrong_source.source.path = root.join("one.bin").display().to_string();
        assert!(
            wrong_source
                .with_write_preflight(&plan, &warning, true)
                .unwrap_err()
                .contains("不一致")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_preflight_accepts_multiple_explicit_source_roots() {
        let base = std::env::temp_dir().join(format!(
            "tapecpy-multi-preflight-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = base.join("first");
        let second = base.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("one.bin"), b"1").unwrap();
        fs::write(second.join("two.bin"), b"22").unwrap();
        let plan = crate::app::scan_source_roots(&[first.clone(), second.clone()]).unwrap();
        let capacity = crate::app::assess_write_capacity(plan.payload_bytes, Some(10), "sampled");
        let endpoints = [first, second]
            .into_iter()
            .map(|path| Endpoint {
                path: path.display().to_string(),
                filesystem_type: None,
                mount_source: None,
            })
            .collect::<Vec<_>>();
        let value = spec(OperationKind::Write)
            .with_source_roots(endpoints)
            .with_write_preflight(&plan, &capacity, false)
            .unwrap();
        assert_eq!(value.source_roots.len(), 2);
        assert_eq!(value.write_preflight.unwrap().roots.len(), 2);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn legacy_job_spec_defaults_to_no_write_preflight() {
        let value = serde_json::to_value(spec(OperationKind::Write)).unwrap();
        let mut object = value.as_object().unwrap().clone();
        object.remove("write_preflight");
        object.remove("source_roots");
        let decoded: JobSpec = serde_json::from_value(object.into()).unwrap();
        assert!(decoded.write_preflight.is_none());
        assert!(decoded.source_roots.is_empty());
    }

    #[test]
    fn legacy_job_spec_defaults_to_keep_loaded() {
        let value = serde_json::to_value(spec(OperationKind::Write)).unwrap();
        let mut object = value.as_object().unwrap().clone();
        object.remove("completion_action");
        object.remove("volume_barcode");
        object.remove("volume_name");
        let decoded: JobSpec = serde_json::from_value(object.into()).unwrap();
        assert_eq!(decoded.completion_action, CompletionAction::KeepLoaded);
        assert!(decoded.volume_barcode.is_none());
        assert!(decoded.volume_name.is_none());
    }

    #[test]
    fn application_completed_event_waits_for_runner_completion_action() {
        let mut state = JobState::queued(spec(OperationKind::Write)).unwrap();
        let event = WriteEvent {
            phase: WritePhase::Completed,
            current_file: None,
            files_completed: 1,
            files_total: 1,
            bytes_written: 10,
            bytes_total: 10,
            partition: Some(1),
            logical_block: Some(20),
            telemetry: None,
            performance: None,
            failure: None,
        };
        state.apply_write_event(&event, "done".into());
        assert_eq!(state.phase, JobPhase::Finalizing);
        assert!(!state.phase.is_terminal());
    }

    #[test]
    fn state_is_persisted_atomically_and_round_trips() {
        let paths = test_paths("persist");
        let state = JobState::queued(spec(OperationKind::Write)).unwrap();
        save_spec(&paths, &state.spec).unwrap();
        save_state(&paths, &state).unwrap();
        assert_eq!(load_spec(&paths).unwrap(), state.spec);
        assert_eq!(load_state(&paths).unwrap(), state);
        assert_eq!(
            fs::metadata(&paths.directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = fs::remove_dir_all(paths.directory.parent().unwrap());
    }

    #[test]
    fn ipc_status_watch_and_cancel_share_one_snapshot() {
        let paths = test_paths("ipc");
        paths.create().unwrap();
        let token = CancellationToken::default();
        let control = JobControl::new(
            paths.clone(),
            JobState::queued(spec(OperationKind::Write)).unwrap(),
            token.clone(),
        )
        .unwrap();

        let response = handle_request(
            Request::Status {
                protocol_version: PROTOCOL_VERSION,
            },
            &control,
        );
        assert!(matches!(response, Response::State { .. }));

        let response = handle_request(
            Request::Cancel {
                protocol_version: PROTOCOL_VERSION,
            },
            &control,
        );
        let Response::CancelAccepted { state } = response else {
            panic!("unexpected response")
        };
        assert_eq!(state.phase, JobPhase::CancellationRequested);
        assert!(token.is_cancelled());
        assert_eq!(
            load_state(&paths).unwrap().phase,
            JobPhase::CancellationRequested
        );

        let _ = fs::remove_dir_all(paths.directory.parent().unwrap());
    }

    #[test]
    fn dead_active_runner_becomes_interrupted_without_resume() {
        let mut state = JobState::queued(spec(OperationKind::Write)).unwrap();
        state.phase = JobPhase::Running;
        state.runner_pid = Some(42);
        let state = reconcile_interrupted(state, |_| false);
        assert_eq!(state.phase, JobPhase::Interrupted);
        assert!(state.requires_diagnosis);
    }

    #[test]
    fn cancellation_request_survives_progress_until_terminal_event() {
        let mut state = JobState::queued(spec(OperationKind::Write)).unwrap();
        state.phase = JobPhase::CancellationRequested;
        state.apply_write_event(
            &WriteEvent {
                phase: WritePhase::WritingData,
                current_file: Some("/still-finishing-record".into()),
                files_completed: 0,
                files_total: 1,
                bytes_written: 512,
                bytes_total: 1024,
                partition: Some(1),
                logical_block: Some(6),
                telemetry: None,
                performance: None,
                failure: None,
            },
            "later".into(),
        );
        assert_eq!(state.phase, JobPhase::CancellationRequested);
    }

    #[test]
    fn write_telemetry_snapshot_retains_channels_worst_and_bounded_history() {
        let mut state = JobState::queued(spec(OperationKind::Write)).unwrap();
        for index in 0..crate::app::PERFORMANCE_HISTORY_CAPACITY + 2 {
            let rate = if index == 0 { -3.0 } else { -6.0 };
            state.apply_write_event(
                &WriteEvent {
                    phase: WritePhase::WritingData,
                    current_file: Some("/sample".into()),
                    files_completed: 0,
                    files_total: 1,
                    bytes_written: index as u64,
                    bytes_total: 1_000,
                    partition: Some(1),
                    logical_block: Some(index as u64),
                    telemetry: Some(crate::app::ChannelTelemetrySample {
                        elapsed_millis: index as u64 * 5_000,
                        timestamp: format!("sample-{index}"),
                        partition: 1,
                        logical_block: index as u64,
                        throughput_bytes_per_second: index as f64,
                        channel_rates: vec![crate::device::channel_error::ChannelRate {
                            channel: 4,
                            log10_bit_error_rate: Some(rate),
                            ccp_advanced: true,
                        }],
                        worst_rate: Some(rate),
                    }),
                    performance: Some(crate::app::WritePerformanceSample {
                        timestamp: format!("sample-{index}"),
                        source_bytes_per_second: index as f64 + 1.0,
                        tape_bytes_per_second: index as f64,
                        buffer_used_bytes: 4,
                        buffer_capacity_bytes: 8,
                        reader_waiting: false,
                        writer_waiting: false,
                    }),
                    failure: None,
                },
                format!("sample-{index}"),
            );
        }
        assert_eq!(state.progress.channel_rates[0].channel, 4);
        assert_eq!(state.progress.session_worst_channel, Some(4));
        assert_eq!(state.progress.session_worst_channel_rate, Some(-3.0));
        assert_eq!(
            state.progress.throughput_history.len(),
            crate::app::PERFORMANCE_HISTORY_CAPACITY
        );
        assert_eq!(state.progress.throughput_history[0].bytes_per_second, 2.0);
        assert_eq!(state.progress.source_bytes_per_second, Some(602.0));
        assert_eq!(state.progress.buffer_used_bytes, Some(4));
    }

    #[test]
    fn protocol_version_mismatch_is_explicit() {
        let paths = test_paths("version");
        let control = JobControl::new(
            paths.clone(),
            JobState::queued(spec(OperationKind::Write)).unwrap(),
            CancellationToken::default(),
        )
        .unwrap();
        let response = handle_request(
            Request::Status {
                protocol_version: PROTOCOL_VERSION + 1,
            },
            &control,
        );
        let Response::Error { message } = response else {
            panic!("version mismatch was accepted")
        };
        assert!(message.contains("version"));
        let _ = fs::remove_dir_all(paths.directory.parent().unwrap());
    }

    #[test]
    fn job_listing_ignores_non_jobs_and_orders_latest_first() {
        let first = test_paths("listing");
        let root = first.directory.parent().unwrap();
        let mut first_state = JobState::queued(spec(OperationKind::Read)).unwrap();
        first_state.updated_at = "2026-08-10T00:00:00Z".into();
        save_state(&first, &first_state).unwrap();

        let second_id = JobId::parse("second-job").unwrap();
        let second = JobPaths::new(root, &second_id);
        let mut second_spec = spec(OperationKind::Write);
        second_spec.id = second_id;
        let mut second_state = JobState::queued(second_spec).unwrap();
        second_state.updated_at = "2026-08-10T01:00:00Z".into();
        save_state(&second, &second_state).unwrap();
        fs::create_dir_all(root.join(".locks")).unwrap();

        let states = list_states(root).unwrap();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].spec.id.as_str(), "second-job");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "local sandbox forbids AF_UNIX bind; run on a Linux integration host"]
    fn unix_socket_transport_round_trips_on_linux_host() {
        let paths = test_paths("socket-transport");
        let control = JobControl::new(
            paths.clone(),
            JobState::queued(spec(OperationKind::Read)).unwrap(),
            CancellationToken::default(),
        )
        .unwrap();
        let _server = IpcServer::start(control).unwrap();
        let response = request(
            &paths.socket,
            &Request::Status {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        let Response::State { state } = response else {
            panic!("unexpected response")
        };
        assert_eq!(state.spec.operation, OperationKind::Read);
        let _ = fs::remove_dir_all(paths.directory.parent().unwrap());
    }
}
