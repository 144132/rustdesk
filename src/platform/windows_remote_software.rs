//! Windows remote-software operation, durable state, and service-only recovery worker.
use crate::remote_software::{
    installer_outcome, retry_delay, validate_manifest, validate_package_url, validate_sha256,
    InstallOutcome, DetectionRule, InstallMode, InstallerType, RemoteSoftwareManifest,
    RemoteSoftwareStage, RemoteSoftwareStatus,
};
use hbb_common::thiserror;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Condvar, Mutex},
    time::Duration,
};

#[cfg(any(windows, test))]
use std::sync::Arc;

#[cfg(windows)]
pub use native::{build_process_command, detect_installed, execute, service_is_available};

// Keep Task 1's validation error contract unchanged; runtime errors never retain URLs.
#[derive(Debug, thiserror::Error)]
pub enum RemoteSoftwareError {
    #[error(transparent)]
    Validation(#[from] crate::remote_software::RemoteSoftwareError),
    #[error("download_failed: {0}")]
    DownloadFailed(&'static str),
    #[error("checksum_mismatch")]
    ChecksumMismatch,
    #[error("download_failed: cache operation failed")]
    Cache,
    #[error("invalid_manifest: {0}")]
    InvalidManifest(&'static str),
    #[error("detection_failed: cannot inspect installed software")]
    Detection,
    #[error("installer_failed: cannot start or wait for installer")]
    Installer,
    #[error("installer_busy")]
    Busy,
    #[error("state_failed")]
    State,
}

const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SELF_HEAL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const ATTEMPT_WINDOW_SECONDS: u64 = 60 * 60;
const WORKER_STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct RecoveryState {
    pub entries: Vec<RecoveryEntry>,
}

#[derive(Deserialize)]
struct RawRecoveryState {
    #[serde(default)]
    entries: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RecoveryEntry {
    pub manifest: RemoteSoftwareManifest,
    pub verified_package_path: PathBuf,
    pub last_result: Option<RemoteSoftwareStatus>,
    pub attempt_timestamps: Vec<u64>,
    pub failure_count: u32,
    pub paused: bool,
}

pub struct StateStore {
    root: PathBuf,
    #[cfg(windows)]
    // Pins ProgramData and every feature ancestor without delete sharing, so later
    // path-based IO cannot be redirected through an ancestor junction swap.
    _cache_guard: Option<native::CacheDirectory>,
}

struct TemporaryStateFile {
    file: Option<File>,
    path: PathBuf,
}

impl Drop for TemporaryStateFile {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

impl StateStore {
    #[cfg(test)]
    fn with_root(root: PathBuf) -> Self {
        Self {
            root,
            #[cfg(windows)]
            _cache_guard: None,
        }
    }

    #[cfg(windows)]
    pub fn production() -> Result<Self, RemoteSoftwareError> {
        Self::from_prepared_cache(native::prepare_cache()?)
    }

    #[cfg(windows)]
    fn from_prepared_cache(
        cache_guard: native::CacheDirectory,
    ) -> Result<Self, RemoteSoftwareError> {
        let root = cache_guard.state_root()?;
        Ok(Self {
            root,
            _cache_guard: Some(cache_guard),
        })
    }

    pub fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn package_root(&self) -> PathBuf {
        self.root.join("packages")
    }

    pub fn load(&self) -> Result<RecoveryState, RemoteSoftwareError> {
        let bytes = match fs::read(self.state_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RecoveryState::default());
            }
            Err(_) => return Err(RemoteSoftwareError::State),
        };
        let raw: RawRecoveryState = match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(_) => return Ok(RecoveryState::default()),
        };
        let entries = raw
            .entries
            .into_iter()
            .filter_map(|value| serde_json::from_value::<RecoveryEntry>(value).ok())
            .filter(|entry| self.validate_entry(entry).is_ok())
            .collect();
        Ok(RecoveryState { entries })
    }

    pub fn save(&self, state: &RecoveryState) -> Result<(), RemoteSoftwareError> {
        for entry in &state.entries {
            self.validate_entry(entry)?;
        }
        let temporary_path = self.root.join(format!(
            ".state.json.{}.tmp",
            uuid::Uuid::new_v4()
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(|_| RemoteSoftwareError::State)?;
        let mut temporary = TemporaryStateFile {
            file: Some(file),
            path: temporary_path,
        };
        let file = temporary.file.as_mut().ok_or(RemoteSoftwareError::State)?;
        serde_json::to_writer_pretty(&mut *file, state).map_err(|_| RemoteSoftwareError::State)?;
        file.flush().map_err(|_| RemoteSoftwareError::State)?;
        file.sync_all().map_err(|_| RemoteSoftwareError::State)?;
        drop(temporary.file.take());
        atomic_replace(&temporary.path, &self.state_path())?;
        Ok(())
    }

    pub fn remove_stale_part_files(&self) -> Result<(), RemoteSoftwareError> {
        self.remove_stale_part_files_with(|_, metadata| reject_reparse(metadata))
    }

    fn remove_stale_part_files_with(
        &self,
        mut inspect: impl FnMut(&Path, &fs::Metadata) -> Result<(), RemoteSoftwareError>,
    ) -> Result<(), RemoteSoftwareError> {
        let package_root = self.package_root();
        let metadata = match fs::symlink_metadata(&package_root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(RemoteSoftwareError::State),
        };
        reject_reparse(&metadata)?;
        if !metadata.is_dir() {
            return Err(RemoteSoftwareError::State);
        }
        let entries = match fs::read_dir(&package_root) {
            Ok(entries) => entries,
            Err(_) => return Err(RemoteSoftwareError::State),
        };
        for entry in entries {
            let entry = entry.map_err(|_| RemoteSoftwareError::State)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|_| RemoteSoftwareError::State)?;
            // Inspect every direct child before considering its name or deleting it.
            inspect(&path, &metadata)?;
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".part"))
            {
                continue;
            }
            if metadata.is_file() {
                fs::remove_file(path).map_err(|_| RemoteSoftwareError::State)?;
            }
        }
        Ok(())
    }

    fn validate_entry(&self, entry: &RecoveryEntry) -> Result<(), RemoteSoftwareError> {
        validate_recovery_entry(&self.package_root(), entry)
    }
}

trait WorkerStateStore {
    fn remove_stale_part_files(&mut self) -> Result<(), RemoteSoftwareError>;
    fn load(&mut self) -> Result<RecoveryState, RemoteSoftwareError>;
    fn save(&mut self, state: &RecoveryState) -> Result<(), RemoteSoftwareError>;
    fn package_root(&self) -> PathBuf;
}

impl WorkerStateStore for StateStore {
    fn remove_stale_part_files(&mut self) -> Result<(), RemoteSoftwareError> {
        StateStore::remove_stale_part_files(self)
    }

    fn load(&mut self) -> Result<RecoveryState, RemoteSoftwareError> {
        StateStore::load(self)
    }

    fn save(&mut self, state: &RecoveryState) -> Result<(), RemoteSoftwareError> {
        StateStore::save(self, state)
    }

    fn package_root(&self) -> PathBuf {
        StateStore::package_root(self)
    }
}

fn validate_recovery_entry(
    package_root: &Path,
    entry: &RecoveryEntry,
) -> Result<(), RemoteSoftwareError> {
    validate_manifest(&entry.manifest)?;
    let (_, expected) = cache_paths(package_root, &entry.manifest)?;
    if entry.verified_package_path != expected {
        return Err(RemoteSoftwareError::State);
    }
    Ok(())
}

fn validate_detection(rule: &DetectionRule) -> Result<(), RemoteSoftwareError> {
    validate_manifest(&RemoteSoftwareManifest {
        request_id: "detection".into(),
        software_name: "detection".into(),
        package_url: "https://szxinyu.com/detection.exe".into(),
        sha256: "00".repeat(32),
        installer_type: InstallerType::Exe,
        detection_rule: rule.clone(),
        silent_args: Vec::new(),
        mode: InstallMode::DownloadOnly,
    })?;
    if let DetectionRule::MsiProductCode(code) = rule {
        let bytes = code.as_bytes();
        if bytes.len() != 38
            || bytes[0] != b'{'
            || bytes[37] != b'}'
            || bytes[1..37].iter().enumerate().any(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    *byte != b'-'
                } else {
                    !byte.is_ascii_hexdigit()
                }
            })
        {
            return Err(RemoteSoftwareError::InvalidManifest("invalid MSI ProductCode"));
        }
    }
    Ok(())
}

#[derive(Default)]
struct WorkerStop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct WorkerCompletion {
    finished: Mutex<bool>,
    wake: Condvar,
}

#[cfg(any(windows, test))]
impl WorkerCompletion {
    fn finish(&self) {
        if let Ok(mut finished) = self.finished.lock() {
            *finished = true;
            self.wake.notify_all();
        }
    }

    fn wait(&self, duration: Duration) -> bool {
        let finished = match self.finished.lock() {
            Ok(finished) => finished,
            Err(_) => return true,
        };
        if *finished {
            return true;
        }
        self.wake
            .wait_timeout_while(finished, duration, |finished| !*finished)
            .map(|(finished, _)| *finished)
            .unwrap_or(true)
    }
}

#[cfg(any(windows, test))]
struct WorkerCompletionGuard {
    completion: Arc<WorkerCompletion>,
}

#[cfg(any(windows, test))]
impl Drop for WorkerCompletionGuard {
    fn drop(&mut self) {
        self.completion.finish();
    }
}

#[cfg(any(windows, test))]
fn stop_worker_thread(
    stop: &WorkerStop,
    completion: &WorkerCompletion,
    thread: Option<std::thread::JoinHandle<()>>,
    timeout: Duration,
) {
    stop.stop();
    if let Some(thread) = thread {
        if completion.wait(timeout) {
            let _ = thread.join();
        }
        // A still-running thread owns only its stop/completion Arcs, so dropping
        // the JoinHandle detaches it without blocking the service control thread.
    }
}

impl WorkerStop {
    fn stop(&self) {
        if let Ok(mut stopped) = self.stopped.lock() {
            *stopped = true;
            self.wake.notify_all();
        }
    }

    fn is_stopped(&self) -> bool {
        self.stopped.lock().map(|stopped| *stopped).unwrap_or(true)
    }

    fn try_begin_execution(&self) -> Option<WorkerExecutionGuard<'_>> {
        let stopped = self.stopped.lock().ok()?;
        if *stopped {
            None
        } else {
            Some(WorkerExecutionGuard { _stop: self })
        }
    }

    fn wait(&self, duration: Duration) -> bool {
        let stopped = match self.stopped.lock() {
            Ok(stopped) => stopped,
            Err(_) => return false,
        };
        if *stopped {
            return false;
        }
        self.wake
            .wait_timeout_while(stopped, duration, |stopped| !*stopped)
            .map(|(stopped, _)| !*stopped)
            .unwrap_or(false)
    }
}

struct WorkerExecutionGuard<'a> {
    _stop: &'a WorkerStop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptDecision {
    Ready,
    WaitUntil(u64),
    Paused,
}

fn recent_attempts(entry: &RecoveryEntry, now: u64) -> impl Iterator<Item = u64> + '_ {
    entry
        .attempt_timestamps
        .iter()
        .copied()
        .filter(move |timestamp| *timestamp > now || now - *timestamp < ATTEMPT_WINDOW_SECONDS)
}

fn attempt_decision(entry: &RecoveryEntry, now: u64) -> AttemptDecision {
    if entry.paused {
        return AttemptDecision::Paused;
    }
    if recent_attempts(entry, now).count() >= 3 {
        return AttemptDecision::Paused;
    }
    let retry_due = if entry.failure_count == 0 {
        None
    } else {
        let delay = match retry_delay(entry.failure_count - 1) {
            Some(delay) => delay.as_secs(),
            None => return AttemptDecision::Paused,
        };
        recent_attempts(entry, now)
            .max()
            .map(|last| last.saturating_add(delay))
    };
    if let Some(due) = retry_due {
        if now < due {
            return AttemptDecision::WaitUntil(due);
        }
    }
    AttemptDecision::Ready
}

fn record_attempt_started(entry: &mut RecoveryEntry, now: u64) {
    entry.attempt_timestamps.push(now);
    entry.failure_count = entry.failure_count.saturating_add(1);
    entry.paused = false;
    entry.last_result = Some(RemoteSoftwareStatus {
        request_id: entry.manifest.request_id.clone(),
        stage: RemoteSoftwareStage::Queued,
        message: "self_heal_attempt".into(),
        exit_code: None,
        needs_reboot: false,
        progress_percent: None,
    });
}

fn result_succeeded(stage: &RemoteSoftwareStage) -> bool {
    matches!(
        stage,
        RemoteSoftwareStage::AlreadyInstalled
            | RemoteSoftwareStage::Success
            | RemoteSoftwareStage::NeedsReboot
    )
}

fn apply_execution_result(entry: &mut RecoveryEntry, status: RemoteSoftwareStatus, now: u64) {
    if result_succeeded(&status.stage) {
        entry.failure_count = 0;
        entry.attempt_timestamps.clear();
        entry.paused = false;
    } else if recent_attempts(entry, now).count() >= 3
        || retry_delay(entry.failure_count.saturating_sub(1)).is_none()
    {
        entry.paused = true;
    }
    entry.last_result = Some(status);
}

fn reset_paused_for_restart(entry: &mut RecoveryEntry) -> bool {
    if !entry.paused {
        return false;
    }
    entry.paused = false;
    entry.failure_count = 0;
    entry.attempt_timestamps.clear();
    true
}

fn installed_status(request_id: &str) -> RemoteSoftwareStatus {
    RemoteSoftwareStatus {
        request_id: request_id.into(),
        stage: RemoteSoftwareStage::AlreadyInstalled,
        message: "already_installed".into(),
        exit_code: None,
        needs_reboot: false,
        progress_percent: Some(100),
    }
}

fn missing_terminal_status(request_id: &str) -> RemoteSoftwareStatus {
    RemoteSoftwareStatus {
        request_id: request_id.into(),
        stage: RemoteSoftwareStage::Failed,
        message: "installer_failed: missing terminal status".into(),
        exit_code: None,
        needs_reboot: false,
        progress_percent: None,
    }
}

fn run_worker_pass<S, D, E>(
    store: &mut S,
    state: &mut RecoveryState,
    stop: &WorkerStop,
    now: u64,
    detect: &mut D,
    execute_entry: &mut E,
) -> Result<(), RemoteSoftwareError>
where
    S: WorkerStateStore,
    D: FnMut(&DetectionRule) -> Result<bool, RemoteSoftwareError>,
    E: FnMut(RemoteSoftwareManifest) -> Option<RemoteSoftwareStatus>,
{
    if stop.is_stopped() {
        return Ok(());
    }
    let package_root = store.package_root();
    let count = state.entries.len();
    state
        .entries
        .retain(|entry| {
            validate_recovery_entry(&package_root, entry).is_ok()
                && validate_detection(&entry.manifest.detection_rule).is_ok()
        });
    if state.entries.len() != count {
        store.save(state)?;
    }
    for index in 0..state.entries.len() {
        if stop.is_stopped() {
            break;
        }
        let before = state.entries[index].attempt_timestamps.len();
        state.entries[index]
            .attempt_timestamps
            .retain(|timestamp| *timestamp > now || now - *timestamp < ATTEMPT_WINDOW_SECONDS);
        if state.entries[index].attempt_timestamps.len() != before {
            store.save(state)?;
        }
        let detection = detect(&state.entries[index].manifest.detection_rule);
        match detection {
            Ok(true) => {
                let status = installed_status(&state.entries[index].manifest.request_id);
                apply_execution_result(&mut state.entries[index], status, now);
                store.save(state)?;
                continue;
            }
            Err(error) => {
                state.entries[index].last_result = Some(RemoteSoftwareStatus {
                    request_id: state.entries[index].manifest.request_id.clone(),
                    stage: RemoteSoftwareStage::Failed,
                    message: error.to_string(),
                    exit_code: None,
                    needs_reboot: false,
                    progress_percent: None,
                });
                store.save(state)?;
                continue;
            }
            Ok(false) => {}
        }
        if stop.is_stopped() {
            break;
        }
        match attempt_decision(&state.entries[index], now) {
            AttemptDecision::WaitUntil(_) => continue,
            AttemptDecision::Paused => {
                if !state.entries[index].paused {
                    state.entries[index].paused = true;
                    store.save(state)?;
                }
                continue;
            }
            AttemptDecision::Ready => {}
        }
        record_attempt_started(&mut state.entries[index], now);
        let manifest = state.entries[index].manifest.clone();
        store.save(state)?;
        let _execution = match stop.try_begin_execution() {
            Some(execution) => execution,
            None => break,
        };
        let status = execute_entry(manifest.clone())
            .filter(|status| result_succeeded(&status.stage) || status.stage == RemoteSoftwareStage::Failed)
            .unwrap_or_else(|| missing_terminal_status(&manifest.request_id));
        apply_execution_result(&mut state.entries[index], status, now);
        store.save(state)?;
    }
    Ok(())
}

fn run_worker_loop<S, C, W, D, E>(
    store: &mut S,
    stop: &WorkerStop,
    mut now: C,
    mut wait: W,
    mut detect: D,
    mut execute_entry: E,
) -> Result<(), RemoteSoftwareError>
where
    S: WorkerStateStore,
    C: FnMut() -> u64,
    W: FnMut(Duration, &WorkerStop) -> bool,
    D: FnMut(&DetectionRule) -> Result<bool, RemoteSoftwareError>,
    E: FnMut(RemoteSoftwareManifest) -> Option<RemoteSoftwareStatus>,
{
    if stop.is_stopped() {
        return Ok(());
    }
    store.remove_stale_part_files()?;
    let mut state = store.load()?;
    let mut restarted = false;
    for entry in &mut state.entries {
        restarted |= reset_paused_for_restart(entry);
    }
    if restarted {
        store.save(&state)?;
    }
    run_worker_pass(
        store,
        &mut state,
        stop,
        now(),
        &mut detect,
        &mut execute_entry,
    )?;
    while wait(SELF_HEAL_INTERVAL, stop) {
        if stop.is_stopped() {
            break;
        }
        run_worker_pass(
            store,
            &mut state,
            stop,
            now(),
            &mut detect,
            &mut execute_entry,
        )?;
    }
    Ok(())
}

fn worker_entry_allowed(
    windows: bool,
    installed: bool,
    service_available: bool,
    args: &[OsString],
) -> bool {
    windows
        && installed
        && service_available
        && args.len() == 1
        && args[0] == OsStr::new("--service")
}

#[cfg(windows)]
static SELF_HEAL_WORKER_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
struct WorkerRunningGuard;

#[cfg(windows)]
impl Drop for WorkerRunningGuard {
    fn drop(&mut self) {
        SELF_HEAL_WORKER_RUNNING.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(any(windows, test))]
pub struct SelfHealWorkerHandle {
    stop: Arc<WorkerStop>,
    completion: Arc<WorkerCompletion>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(any(windows, test))]
impl SelfHealWorkerHandle {
    pub fn stop(&mut self) {
        self.stop_inner(WORKER_STOP_JOIN_TIMEOUT);
    }

    fn stop_inner(&mut self, timeout: Duration) {
        stop_worker_thread(
            &self.stop,
            &self.completion,
            self.thread.take(),
            timeout,
        );
    }
}

#[cfg(any(windows, test))]
impl Drop for SelfHealWorkerHandle {
    fn drop(&mut self) {
        self.stop_inner(WORKER_STOP_JOIN_TIMEOUT);
    }
}

#[cfg(windows)]
pub fn start_self_heal_worker() -> Result<Option<SelfHealWorkerHandle>, RemoteSoftwareError> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !worker_entry_allowed(
        true,
        crate::platform::is_installed(),
        service_is_available(),
        &args,
    ) {
        return Ok(None);
    }
    if SELF_HEAL_WORKER_RUNNING
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::Acquire,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        return Ok(None);
    }
    let mut store = match StateStore::production() {
        Ok(store) => store,
        Err(error) => {
            SELF_HEAL_WORKER_RUNNING.store(false, std::sync::atomic::Ordering::Release);
            return Err(error);
        }
    };
    let stop = Arc::new(WorkerStop::default());
    let worker_stop = stop.clone();
    let completion = Arc::new(WorkerCompletion::default());
    let worker_completion = completion.clone();
    let thread = match std::thread::Builder::new()
        .name("remote-software-self-heal".into())
        .spawn(move || {
            let _completion = WorkerCompletionGuard {
                completion: worker_completion,
            };
            let _running = WorkerRunningGuard;
            let runtime = match hbb_common::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => {
                    hbb_common::log::error!("remote software self-heal runtime unavailable");
                    return;
                }
            };
            let result = run_worker_loop(
                &mut store,
                &worker_stop,
                || {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_secs())
                        .unwrap_or(0)
                },
                |duration, stop| stop.wait(duration),
                native::detect_installed,
                |manifest| {
                    let last = Mutex::new(None);
                    runtime.block_on(native::execute(manifest, |status| {
                        if let Ok(mut last) = last.lock() {
                            *last = Some(status);
                        }
                    }));
                    last.into_inner().ok().flatten()
                },
            );
            if result.is_err() {
                hbb_common::log::error!("remote software self-heal worker stopped after state error");
            }
        })
    {
        Ok(thread) => thread,
        Err(_) => {
            SELF_HEAL_WORKER_RUNNING.store(false, std::sync::atomic::Ordering::Release);
            return Err(RemoteSoftwareError::State);
        }
    };
    Ok(Some(SelfHealWorkerHandle {
        stop,
        completion,
        thread: Some(thread),
    }))
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> Result<(), RemoteSoftwareError> {
    fs::rename(source, destination).map_err(|_| RemoteSoftwareError::State)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> Result<(), RemoteSoftwareError> {
    native::atomic_replace(source, destination)
}

// The Windows entry point supplies the OS operations; this owns all launch/skip decisions.
// Prepared resources (including cache locks) stay alive until the launch closure returns.
fn run_with<P>(
    manifest: &RemoteSoftwareManifest,
    progress: &impl Fn(RemoteSoftwareStage, Option<u8>),
    mut detect: impl FnMut(&DetectionRule) -> Result<bool, RemoteSoftwareError>,
    prepare: impl FnOnce() -> Result<P, RemoteSoftwareError>,
    launch: impl FnOnce(&P) -> Result<Option<i32>, RemoteSoftwareError>,
) -> Result<(RemoteSoftwareStage, Option<i32>), RemoteSoftwareError> {
    validate_manifest(manifest)?;
    if manifest.installer_type == InstallerType::Msi && !manifest.silent_args.is_empty() {
        return Err(RemoteSoftwareError::InvalidManifest("MSI arguments are fixed"));
    }
    if manifest.mode == InstallMode::DownloadAndInstall {
        progress(RemoteSoftwareStage::Detecting, None);
        if detect(&manifest.detection_rule)? {
            return Ok((RemoteSoftwareStage::AlreadyInstalled, None));
        }
    }
    let prepared = prepare()?;
    if manifest.mode == InstallMode::DownloadOnly {
        return Ok((RemoteSoftwareStage::Success, None));
    }
    // Another installer may have completed during a long download.
    progress(RemoteSoftwareStage::Detecting, None);
    if detect(&manifest.detection_rule)? {
        return Ok((RemoteSoftwareStage::AlreadyInstalled, None));
    }
    progress(RemoteSoftwareStage::Installing, None);
    let code = launch(&prepared)?;
    let stage = match installer_outcome(code) {
        InstallOutcome::Success => RemoteSoftwareStage::Success,
        InstallOutcome::NeedsReboot => RemoteSoftwareStage::NeedsReboot,
        InstallOutcome::Failed => RemoteSoftwareStage::Failed,
    };
    Ok((stage, code))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistryView {
    Registry64,
    Registry32,
}

fn detect_registry_views(
    rule: &DetectionRule,
    mut inspect: impl FnMut(RegistryView, &DetectionRule) -> Result<bool, RemoteSoftwareError>,
) -> Result<bool, RemoteSoftwareError> {
    for view in [RegistryView::Registry64, RegistryView::Registry32] {
        if inspect(view, rule)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn reject_reparse_attributes(attributes: u32) -> Result<(), RemoteSoftwareError> {
    if attributes & 0x400 != 0 {
        return Err(RemoteSoftwareError::Cache);
    }
    Ok(())
}

fn check_trusted_owner(is_system: bool, is_administrator: bool) -> Result<(), RemoteSoftwareError> {
    if !is_system && !is_administrator {
        return Err(RemoteSoftwareError::Cache);
    }
    Ok(())
}

struct PackageResponse<R> {
    url: url::Url,
    status: u16,
    location: Option<String>,
    length: Option<u64>,
    body: R,
}

fn download_with<R: Read>(
    manifest: &RemoteSoftwareManifest,
    part: &Path,
    package: &Path,
    progress: &impl Fn(RemoteSoftwareStage, Option<u8>),
    mut request: impl FnMut(&url::Url) -> Result<PackageResponse<R>, RemoteSoftwareError>,
) -> Result<(), RemoteSoftwareError> {
    let mut url = validate_package_url(&manifest.package_url)?;
    let mut followed = 0;
    loop {
        validate_package_url(url.as_str())?;
        let response = request(&url)?;
        validate_package_url(response.url.as_str())?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            let location = response.location.as_deref()
                .ok_or(RemoteSoftwareError::DownloadFailed("redirect missing Location"))?;
            url = checked_redirect(&response.url, location, followed)?;
            followed += 1;
            continue;
        }
        if response.status != 200 {
            return Err(RemoteSoftwareError::DownloadFailed("HTTP status rejected"));
        }
        let length = response.length;
        if let Some(length) = length {
            checked_size(0, length)?;
        }
        let last = std::cell::Cell::new(None);
        write_verified(response.body, part, package, &manifest.sha256, |bytes| {
            let percent = length.filter(|length| *length > 0)
                .map(|length| ((bytes.saturating_mul(100) / length).min(99)) as u8);
            if percent != last.get() {
                progress(RemoteSoftwareStage::Downloading, percent);
                last.set(percent);
            }
        })?;
        return Ok(());
    }
}

fn checked_size(current: u64, additional: u64) -> Result<u64, RemoteSoftwareError> {
    current
        .checked_add(additional)
        .filter(|size| *size <= MAX_PACKAGE_BYTES)
        .ok_or(RemoteSoftwareError::DownloadFailed("package exceeds 2 GiB"))
}

fn checked_redirect(
    current: &url::Url,
    location: &str,
    followed: usize,
) -> Result<url::Url, RemoteSoftwareError> {
    if followed >= 5 {
        return Err(RemoteSoftwareError::DownloadFailed("too many redirects"));
    }
    // URL joining normalizes raw control characters; reject them before normalization.
    if location.chars().any(char::is_control) {
        return Err(RemoteSoftwareError::DownloadFailed("invalid redirect"));
    }
    let next = current
        .join(location)
        .map_err(|_| RemoteSoftwareError::DownloadFailed("invalid redirect"))?;
    Ok(validate_package_url(next.as_str())?)
}

fn display_name_matches(actual: &str, expected: &str) -> bool {
    let normalize = |value: &str| value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let expected = normalize(expected);
    !expected.is_empty() && normalize(actual) == expected
}

fn cache_paths(
    directory: &Path,
    manifest: &RemoteSoftwareManifest,
) -> Result<(PathBuf, PathBuf), RemoteSoftwareError> {
    validate_sha256(&manifest.sha256)?;
    let digest = manifest.sha256.to_ascii_lowercase();
    let extension = match manifest.installer_type {
        InstallerType::Msi => "msi",
        InstallerType::Exe => "exe",
    };
    Ok((
        directory.join(format!("{digest}.part")),
        directory.join(format!("{digest}.{extension}")),
    ))
}

fn reject_reparse(metadata: &fs::Metadata) -> Result<(), RemoteSoftwareError> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        reject_reparse_attributes(metadata.file_attributes())?;
    }
    if metadata.file_type().is_symlink() {
        return Err(RemoteSoftwareError::Cache);
    }
    Ok(())
}

fn remove_cache_file(path: &Path) -> Result<(), RemoteSoftwareError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            reject_reparse(&metadata)?;
            if !metadata.is_file() {
                return Err(RemoteSoftwareError::Cache);
            }
            fs::remove_file(path).map_err(|_| RemoteSoftwareError::Cache)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(RemoteSoftwareError::Cache),
    }
}

// Closing the file before unlinking matters on Windows, including unwinding/error paths.
struct PartialDownload {
    file: Option<File>,
    path: PathBuf,
    promoted: bool,
}

impl Drop for PartialDownload {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.promoted && remove_cache_file(&self.path).is_err() {
            hbb_common::log::warn!("remote software partial cache cleanup failed");
        }
    }
}

fn write_verified(
    mut source: impl Read,
    part: &Path,
    package: &Path,
    expected: &str,
    progress: impl Fn(u64),
) -> Result<(), RemoteSoftwareError> {
    let expected = validate_sha256(expected)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(part).map_err(|_| RemoteSoftwareError::Cache)?;
    let mut partial = PartialDownload {
        file: Some(file),
        path: part.to_owned(),
        promoted: false,
    };
    let mut hasher = Sha256::new();
    let mut total = 0;
    let mut buffer = [0u8; 64 * 1024];
    let file = partial.file.as_mut().ok_or(RemoteSoftwareError::Cache)?;
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|_| RemoteSoftwareError::DownloadFailed("response interrupted"))?;
        if count == 0 {
            break;
        }
        total = checked_size(total, count as u64)?;
        file.write_all(&buffer[..count]).map_err(|_| RemoteSoftwareError::Cache)?;
        hasher.update(&buffer[..count]);
        progress(total);
    }
    file.flush().map_err(|_| RemoteSoftwareError::Cache)?;
    file.sync_all().map_err(|_| RemoteSoftwareError::Cache)?;
    drop(partial.file.take());
    if hasher.finalize().as_slice() != expected {
        return Err(RemoteSoftwareError::ChecksumMismatch);
    }
    fs::rename(part, package).map_err(|_| RemoteSoftwareError::Cache)?;
    partial.promoted = true;
    Ok(())
}

// Keep this handle alive until the installer exits: deny write/delete sharing on Windows.
fn verified_cache(path: &Path, expected: &str) -> Result<Option<File>, RemoteSoftwareError> {
    let expected = validate_sha256(expected)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x0020_0000); // READ, OPEN_REPARSE_POINT
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RemoteSoftwareError::Cache),
    };
    let metadata = file.metadata().map_err(|_| RemoteSoftwareError::Cache)?;
    reject_reparse(&metadata)?;
    if !metadata.is_file() {
        return Err(RemoteSoftwareError::Cache);
    }
    let verify = || -> Result<bool, RemoteSoftwareError> {
        checked_size(0, metadata.len())?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0;
        loop {
            let count = file.read(&mut buffer).map_err(|_| RemoteSoftwareError::Cache)?;
            if count == 0 {
                break;
            }
            total = checked_size(total, count as u64)?;
            hasher.update(&buffer[..count]);
        }
        Ok(hasher.finalize().as_slice() == expected)
    };
    let mut verify = verify;
    match verify() {
        Ok(true) => Ok(Some(file)),
        result => {
            drop(file);
            remove_cache_file(path)?;
            result.map(|_| None)
        }
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use crate::remote_software::{DetectionRule, InstallMode, RemoteSoftwareStage, RemoteSoftwareStatus};
    use hbb_common::tokio;
    use std::{
        ffi::OsString,
        os::windows::{ffi::{OsStrExt, OsStringExt}, fs::OpenOptionsExt, io::AsRawHandle, process::CommandExt},
        process::{Command, Stdio},
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };
    use windows::{
        core::{w, PCWSTR},
        Win32::{
            Foundation::{HANDLE, HLOCAL, LocalFree},
            Security::{
                Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT},
                GetSecurityDescriptorDacl, IsWellKnownSid, WinBuiltinAdministratorsSid, WinLocalSystemSid,
                DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
                PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
            },
            Storage::FileSystem::{
                CreateDirectoryW, MoveFileExW, MOVEFILE_REPLACE_EXISTING,
                MOVEFILE_WRITE_THROUGH,
            },
            System::{Com::CoTaskMemFree, SystemInformation::GetSystemDirectoryW},
            UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath, KF_FLAG_DEFAULT},
        },
    };
    use winreg::{enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY}, RegKey};

    const UNINSTALL_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    static EXECUTING: AtomicBool = AtomicBool::new(false);

    struct ExecutionLease;
    impl Drop for ExecutionLease {
        fn drop(&mut self) {
            EXECUTING.store(false, Ordering::Release);
        }
    }

    pub fn service_is_available() -> bool {
        super::super::windows::is_self_service_running()
    }

    pub(super) fn atomic_replace(
        source: &Path,
        destination: &Path,
    ) -> Result<(), RemoteSoftwareError> {
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|_| RemoteSoftwareError::State)
    }

    pub fn detect_installed(rule: &DetectionRule) -> Result<bool, RemoteSoftwareError> {
        super::validate_detection(rule)?;
        if let DetectionRule::ExePath(path) = rule {
            return Ok(Path::new(path).is_file());
        }
        let machine = RegKey::predef(HKEY_LOCAL_MACHINE);
        detect_registry_views(rule, |view, rule| {
            let view = match view {
                RegistryView::Registry64 => KEY_WOW64_64KEY,
                RegistryView::Registry32 => KEY_WOW64_32KEY,
            };
            let uninstall = match machine.open_subkey_with_flags(UNINSTALL_KEY, KEY_READ | view) {
                Ok(key) => key,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err(RemoteSoftwareError::Detection),
            };
            if let DetectionRule::MsiProductCode(code) = rule {
                match uninstall.open_subkey_with_flags(code, KEY_READ | view) {
                    Ok(_) => return Ok(true),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                    Err(_) => return Err(RemoteSoftwareError::Detection),
                }
            }
            if let DetectionRule::UninstallDisplayName(expected) = rule {
                for name in uninstall.enum_keys() {
                    let name = name.map_err(|_| RemoteSoftwareError::Detection)?;
                    let entry = match uninstall.open_subkey_with_flags(name, KEY_READ | view) {
                        Ok(entry) => entry,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(_) => return Err(RemoteSoftwareError::Detection),
                    };
                    match entry.get_value::<String, _>("DisplayName") {
                        Ok(actual) if display_name_matches(&actual, expected) => return Ok(true),
                        Ok(_) => {},
                        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::InvalidData) => {},
                        Err(_) => return Err(RemoteSoftwareError::Detection),
                    }
                }
            }
            Ok(false)
        })
    }

    pub fn build_process_command(
        manifest: &RemoteSoftwareManifest,
        package: &Path,
    ) -> Result<Command, RemoteSoftwareError> {
        validate_manifest(manifest)?;
        let extension = match manifest.installer_type { InstallerType::Msi => "msi", InstallerType::Exe => "exe" };
        if !package.is_absolute()
            || !package.extension().and_then(|value| value.to_str()).is_some_and(|value| value.eq_ignore_ascii_case(extension))
        {
            return Err(RemoteSoftwareError::InvalidManifest("invalid verified package path"));
        }
        let mut command = match manifest.installer_type {
            InstallerType::Msi => {
                if !manifest.silent_args.is_empty() {
                    return Err(RemoteSoftwareError::InvalidManifest("MSI arguments are fixed"));
                }
                let mut system = vec![0u16; 32768];
                let count = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
                if count == 0 || count >= system.len() { return Err(RemoteSoftwareError::Installer); }
                let system = PathBuf::from(OsString::from_wide(&system[..count]));
                let mut command = Command::new(system.join("msiexec.exe"));
                command.arg("/i").arg(package).args(["/qn", "/norestart"]);
                command
            }
            InstallerType::Exe => {
                let mut command = Command::new(package);
                command.args(&manifest.silent_args);
                command
            }
        };
        command.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        Ok(command)
    }

    struct LocalDescriptor(PSECURITY_DESCRIPTOR);
    impl Drop for LocalDescriptor {
        fn drop(&mut self) {
            unsafe { LocalFree(Some(HLOCAL(self.0.0))); }
        }
    }

    pub(super) struct CacheDirectory {
        path: PathBuf,
        // Deny directory rename/deletion for the entire operation, including installation.
        _handles: Vec<File>,
    }

    impl CacheDirectory {
        pub(super) fn state_root(&self) -> Result<PathBuf, RemoteSoftwareError> {
            self.path
                .parent()
                .map(Path::to_path_buf)
                .ok_or(RemoteSoftwareError::State)
        }
    }

    fn directory_handle(path: &Path, writable_acl: bool) -> Result<File, RemoteSoftwareError> {
        let mut options = OpenOptions::new();
        options.access_mode(0x0002_0080 | if writable_acl { 0x0004_0000 } else { 0 }) // READ_CONTROL | READ_ATTRIBUTES | WRITE_DAC
            .share_mode(3) // READ | WRITE, deliberately no DELETE
            .custom_flags(0x0220_0000); // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        let file = options.open(path).map_err(|_| RemoteSoftwareError::Cache)?;
        let metadata = file.metadata().map_err(|_| RemoteSoftwareError::Cache)?;
        reject_reparse(&metadata)?;
        if !metadata.is_dir() { return Err(RemoteSoftwareError::Cache); }
        Ok(file)
    }

    fn require_trusted_owner(file: &File) -> Result<(), RemoteSoftwareError> {
        let mut owner = PSID::default();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let result = unsafe {
            GetSecurityInfo(HANDLE(file.as_raw_handle()), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION,
                Some(&mut owner), None, None, None, Some(&mut descriptor))
        };
        if result.0 != 0 { return Err(RemoteSoftwareError::Cache); }
        let _descriptor = LocalDescriptor(descriptor);
        if owner.0.is_null() { return Err(RemoteSoftwareError::Cache); }
        check_trusted_owner(
            unsafe { IsWellKnownSid(owner, WinLocalSystemSid).as_bool() },
            unsafe { IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool() },
        )
    }

    fn cache_security_descriptor() -> Result<LocalDescriptor, RemoteSoftwareError> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"), 1, &mut descriptor, None)
        }.map_err(|_| RemoteSoftwareError::Cache)?;
        Ok(LocalDescriptor(descriptor))
    }

    pub(super) fn prepare_cache() -> Result<CacheDirectory, RemoteSoftwareError> {
        let value = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }
            .map_err(|_| RemoteSoftwareError::Cache)?;
        let path = unsafe { value.to_string() };
        unsafe { CoTaskMemFree(Some(value.0.cast())); }
        let mut path = PathBuf::from(path.map_err(|_| RemoteSoftwareError::Cache)?);
        let mut handles = Vec::new();
        // Pin every existing ancestor before following any child path.
        for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            handles.push(directory_handle(ancestor, false)?);
        }
        let descriptor = cache_security_descriptor()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0.0,
            bInheritHandle: false.into(),
        };
        let mut present = Default::default();
        let mut defaulted = Default::default();
        let mut dacl = std::ptr::null_mut();
        unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
            .map_err(|_| RemoteSoftwareError::Cache)?;
        if !present.as_bool() || dacl.is_null() { return Err(RemoteSoftwareError::Cache); }
        for component in ["新育智慧校园", "remote-software", "packages"] {
            path.push(component);
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if let Err(error) = unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&attributes)) } {
                if error.code() != windows::core::HRESULT::from_win32(183) { // ERROR_ALREADY_EXISTS
                    return Err(RemoteSoftwareError::Cache);
                }
            }
            let file = directory_handle(&path, component != "新育智慧校园")?;
            require_trusted_owner(&file)?;
            // Preserve an existing shared brand directory's ACL; restrict only our subtree.
            if component != "新育智慧校园" {
                let result = unsafe {
                    SetSecurityInfo(HANDLE(file.as_raw_handle()), SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        None, None, Some(dacl), None)
                };
                if result.0 != 0 { return Err(RemoteSoftwareError::Cache); }
            }
            handles.push(file);
        }
        Ok(CacheDirectory { path, _handles: handles })
    }

    fn download(
        manifest: &RemoteSoftwareManifest,
        part: &Path,
        package: &Path,
        progress: &impl Fn(RemoteSoftwareStage, Option<u8>),
    ) -> Result<(), RemoteSoftwareError> {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(30 * 60))
            .no_gzip().no_zstd()
            .build().map_err(|_| RemoteSoftwareError::DownloadFailed("HTTP client unavailable"))?;
        download_with(manifest, part, package, progress, |url| {
            let response = client.get(url.clone()).send()
                .map_err(|_| RemoteSoftwareError::DownloadFailed("request failed"))?;
            Ok(PackageResponse {
                url: response.url().clone(),
                status: response.status().as_u16(),
                location: response.headers().get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok()).map(str::to_owned),
                length: response.content_length(),
                body: response,
            })
        })
    }

    fn run(
        manifest: &RemoteSoftwareManifest,
        progress: &impl Fn(RemoteSoftwareStage, Option<u8>),
    ) -> Result<(RemoteSoftwareStage, Option<i32>), RemoteSoftwareError> {
        super::validate_detection(&manifest.detection_rule)?;
        run_with(
            manifest,
            progress,
            detect_installed,
            || prepare_package(manifest, progress),
            |(directory, package, _verified)| {
                let mut command = build_process_command(manifest, package)?;
                command.current_dir(&directory.path);
                let exit = command.status().map_err(|_| RemoteSoftwareError::Installer)?;
                Ok(exit.code())
            },
        )
    }

    fn prepare_package(
        manifest: &RemoteSoftwareManifest,
        progress: &impl Fn(RemoteSoftwareStage, Option<u8>),
    ) -> Result<(CacheDirectory, PathBuf, File), RemoteSoftwareError> {
        let directory = prepare_cache()?;
        let (part, package) = cache_paths(&directory.path, manifest)?;
        remove_cache_file(&part)?;
        progress(RemoteSoftwareStage::Verifying, None);
        let verified = match verified_cache(&package, &manifest.sha256)? {
            Some(file) => file,
            None => {
                progress(RemoteSoftwareStage::Downloading, Some(0));
                download(manifest, &part, &package, progress)?;
                progress(RemoteSoftwareStage::Verifying, None);
                verified_cache(&package, &manifest.sha256)?.ok_or(RemoteSoftwareError::ChecksumMismatch)?
            }
        };
        Ok((directory, package, verified))
    }

    #[cfg(test)]
    mod boundary_tests {
        use super::*;
        use windows::Win32::Security::{GetAce, GetSecurityDescriptorControl, ACCESS_ALLOWED_ACE};

        #[test]
        fn cache_descriptor_grants_only_inheritable_system_and_admin_access() {
            let descriptor = cache_security_descriptor().unwrap();
            let mut control = 0u16;
            let mut revision = 0;
            let mut present = Default::default();
            let mut defaulted = Default::default();
            let mut dacl = std::ptr::null_mut();
            unsafe {
                GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision).unwrap();
                assert_ne!(control & 0x1000, 0, "DACL must be protected from inheritance");
                GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted).unwrap();
                assert!(present.as_bool());
                assert!(!dacl.is_null());
                assert_eq!((*dacl).AceCount, 2);
                let mut system = 0;
                let mut administrators = 0;
                for index in 0..2 {
                    let mut ace = std::ptr::null_mut();
                    GetAce(dacl, index, &mut ace).unwrap();
                    let ace = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                    assert_eq!(ace.Header.AceType, 0, "only allow ACEs");
                    assert_eq!(ace.Header.AceFlags, 3, "inherit to files and directories");
                    assert_eq!(ace.Mask, 0x001f_01ff, "full file access");
                    let sid = PSID(std::ptr::addr_of!(ace.SidStart).cast_mut().cast());
                    if IsWellKnownSid(sid, WinLocalSystemSid).as_bool() { system += 1; }
                    else if IsWellKnownSid(sid, WinBuiltinAdministratorsSid).as_bool() { administrators += 1; }
                    else { panic!("unexpected cache principal"); }
                }
                assert_eq!((system, administrators), (1, 1));
            }
        }

        #[test]
        fn directory_guard_denies_rename_until_released_and_rejects_files() {
            let path = std::env::temp_dir().join(format!("remote-software-guard-{}", uuid::Uuid::new_v4()));
            let renamed = path.with_extension("renamed");
            fs::create_dir(&path).unwrap();
            let guard = directory_handle(&path, false).unwrap();
            assert!(fs::rename(&path, &renamed).is_err());
            drop(guard);
            fs::rename(&path, &renamed).unwrap();
            fs::remove_dir(&renamed).unwrap();
            fs::write(&path, b"not a directory").unwrap();
            assert!(directory_handle(&path, false).is_err());
            fs::remove_file(&path).unwrap();
        }

        #[test]
        fn production_store_pins_intermediate_ancestor_against_junction_swap() {
            let base = std::env::temp_dir().join(format!(
                "remote-software-state-root-{}",
                uuid::Uuid::new_v4()
            ));
            let program_data = base.join("ProgramData");
            let brand = program_data.join("新育智慧校园");
            let feature = brand.join("remote-software");
            let packages = feature.join("packages");
            fs::create_dir_all(&packages).unwrap();
            let handles = [&program_data, &brand, &feature, &packages]
                .into_iter()
                .map(|path| directory_handle(path, false).unwrap())
                .collect();
            let store = StateStore::from_prepared_cache(CacheDirectory {
                path: packages,
                _handles: handles,
            })
            .unwrap();
            let moved = base.join("brand-moved");
            assert!(
                fs::rename(&brand, &moved).is_err(),
                "an intermediate ancestor must not be replaceable by a junction"
            );
            drop(store);
            fs::rename(&brand, &moved).unwrap();
            fs::remove_dir_all(base).unwrap();
        }
    }

    pub async fn execute(
        manifest: RemoteSoftwareManifest,
        progress: impl Fn(RemoteSoftwareStatus) + Send + Sync,
    ) {
        let request_id = manifest.request_id.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        // Blocking IO/process waiting stays off Tokio workers. The operation survives a
        // disconnected observer, and its in-process lease remains held until completion.
        let worker = tokio::task::spawn_blocking(move || {
            let emit = |stage: RemoteSoftwareStage, percent| {
                let status = RemoteSoftwareStatus {
                    request_id: manifest.request_id.clone(), stage, message: String::new(),
                    exit_code: None, needs_reboot: false, progress_percent: percent,
                };
                if sender.send(status).is_err() { /* observer disconnected; finish operation */ }
            };
            let result = if EXECUTING.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                let _lease = ExecutionLease;
                emit(RemoteSoftwareStage::Queued, None);
                run(&manifest, &emit)
            } else { Err(RemoteSoftwareError::Busy) };
            let (stage, exit_code, message) = match result {
                Ok((stage, code)) => {
                    let message = match stage {
                        RemoteSoftwareStage::Failed => "installer_failed",
                        RemoteSoftwareStage::AlreadyInstalled => "already_installed",
                        RemoteSoftwareStage::Success if manifest.mode == InstallMode::DownloadOnly => "downloaded",
                        _ => "",
                    };
                    (stage, code, message.to_owned())
                }
                Err(error) => (RemoteSoftwareStage::Failed, None, error.to_string()),
            };
            let status = RemoteSoftwareStatus {
                request_id: manifest.request_id, needs_reboot: stage == RemoteSoftwareStage::NeedsReboot,
                progress_percent: (stage != RemoteSoftwareStage::Failed).then_some(100),
                stage, message, exit_code,
            };
            if sender.send(status).is_err() { /* observer disconnected */ }
        });
        while let Some(status) = receiver.recv().await { progress(status); }
        if worker.await.is_err() {
            progress(RemoteSoftwareStatus {
                request_id, stage: RemoteSoftwareStage::Failed, message: "installer_failed: worker interrupted".into(),
                exit_code: None, needs_reboot: false, progress_percent: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_software::{
        DetectionRule, InstallMode, InstallerType, RemoteSoftwareStatus,
    };
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::io::{self, Cursor, Read};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Instant;

    fn test_msi_manifest() -> RemoteSoftwareManifest {
        RemoteSoftwareManifest {
            request_id: "task-3a-test".into(),
            software_name: "Example".into(),
            package_url: "https://update.szxinyu.com/office.msi".into(),
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            installer_type: InstallerType::Msi,
            detection_rule: DetectionRule::MsiProductCode(
                "{12345678-1234-1234-1234-123456789ABC}".into(),
            ),
            silent_args: Vec::new(),
            mode: InstallMode::DownloadAndInstall,
        }
    }

    fn unique_test_path(label: &str, extension: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "remote-software-{label}-{}.{}",
            uuid::Uuid::new_v4(),
            extension
        ))
    }

    #[cfg(windows)]
    #[test]
    fn msi_command_uses_msiexec_without_shell() {
        let command = build_process_command(
            &test_msi_manifest(),
            Path::new(r"C:\pkg\office.msi"),
        )
        .unwrap();
        assert_eq!(command.get_program(), Path::new(r"C:\Windows\System32\msiexec.exe"));
        let args: Vec<_> = command.get_args().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(args, vec!["/i", r"C:\pkg\office.msi", "/qn", "/norestart"]);
    }

    #[cfg(windows)]
    #[test]
    fn exe_command_preserves_literal_arguments_and_rejects_control_characters() {
        let mut manifest = test_msi_manifest();
        manifest.installer_type = InstallerType::Exe;
        manifest.package_url = "https://szxinyu.com/app.exe".into();
        manifest.silent_args = vec![
            "/S".into(),
            r"/D=C:\Program Files\R&D".into(),
            r"/Tools=C:\PowerShellTools\pwsh".into(),
            "/Label=a^b|c<d>e".into(),
            r#"/Label=quoted "value""#.into(),
        ];
        let command = build_process_command(&manifest, Path::new(r"C:\pkg\app.exe")).unwrap();
        assert_eq!(command.get_program(), Path::new(r"C:\pkg\app.exe"));
        assert_eq!(command.get_args().collect::<Vec<_>>(), manifest.silent_args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>());
        for argument in ["/S\n", "/S\r", "/S\0", "/S\t"] {
            manifest.silent_args = vec![argument.into()];
            assert!(build_process_command(&manifest, Path::new(r"C:\pkg\app.exe")).is_err());
        }
        manifest.silent_args.clear();
        assert!(build_process_command(&manifest, Path::new(r"C:\pkg\app.cmd")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn deleted_exe_path_is_reported_as_missing() {
        let path = unique_test_path("missing", "exe");
        assert!(!detect_installed(&DetectionRule::ExePath(path.to_string_lossy().into_owned())).unwrap());
        std::fs::write(&path, b"fixture").unwrap();
        assert!(detect_installed(&DetectionRule::ExePath(path.to_string_lossy().into_owned())).unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(!detect_installed(&DetectionRule::ExePath(path.to_string_lossy().into_owned())).unwrap());
        for invalid in [r"\\server\share\app.exe", r"C:app.exe", "relative.exe", "/tmp/app.exe"] {
            assert!(detect_installed(&DetectionRule::ExePath(invalid.into())).is_err());
        }
    }

    #[test]
    fn installer_exit_codes_map_to_reboot_or_failure() {
        assert_eq!(installer_outcome(Some(0)), InstallOutcome::Success);
        assert_eq!(installer_outcome(Some(3010)), InstallOutcome::NeedsReboot);
        assert_eq!(installer_outcome(Some(1603)), InstallOutcome::Failed);
        assert_eq!(installer_outcome(None), InstallOutcome::Failed);
    }

    #[test]
    fn display_name_matching_is_normalized_but_never_partial() {
        assert!(display_name_matches("  Example  APP ", "example app"));
        assert!(!display_name_matches("Example App Helper", "Example App"));
        assert!(!display_name_matches("Example", "Example App"));
        assert!(!display_name_matches(" ", ""));
    }

    #[test]
    fn redirects_enforce_allowlist_and_five_hop_limit() {
        let start = validate_package_url("https://szxinyu.com/app.msi").unwrap();
        assert_eq!(checked_redirect(&start, "/new.msi", 0).unwrap().as_str(), "https://szxinyu.com/new.msi");
        assert!(checked_redirect(&start, "https://update.szxinyu.com/new.msi", 4).is_ok());
        assert!(checked_redirect(&start, "/new.msi", 5).is_err());
        for invalid in ["https://evil-szxinyu.com/app.msi", "http://szxinyu.com/app.msi", "https://u:p@szxinyu.com/app.msi", "https://127.0.0.1/app.msi", "https://szxinyu.com:444/app.msi", "/script.ps1", "/a\n.msi"] {
            assert!(checked_redirect(&start, invalid, 0).is_err());
        }
    }

    #[test]
    fn response_size_limit_includes_unknown_lengths_and_overflow() {
        assert_eq!(checked_size(2_147_483_647, 1).unwrap(), 2_147_483_648);
        assert!(checked_size(2_147_483_648, 1).is_err());
        assert!(checked_size(u64::MAX, 1).is_err());
    }

    struct InterruptedReader;
    impl Read for InterruptedReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::ConnectionReset, "test disconnect"))
        }
    }

    #[test]
    fn verified_download_promotes_only_matching_bytes_and_cleans_failures() {
        let dir = unique_test_path("cache", "dir");
        std::fs::create_dir(&dir).unwrap();
        let manifest = test_msi_manifest();
        let (part, package) = cache_paths(&dir, &manifest).unwrap();
        assert_eq!(part.file_name().unwrap(), format!("{}.part", manifest.sha256).as_str());
        write_verified(Cursor::new(b"abc"), &part, &package, &manifest.sha256, |_| {}).unwrap();
        assert_eq!(std::fs::read(&package).unwrap(), b"abc");
        assert!(!part.exists());
        std::fs::remove_file(&package).unwrap();
        assert!(write_verified(Cursor::new(b"bad"), &part, &package, &manifest.sha256, |_| {}).is_err());
        assert!(!part.exists());
        assert!(!package.exists());
        assert!(write_verified(InterruptedReader, &part, &package, &manifest.sha256, |_| {}).is_err());
        assert!(!part.exists());
        assert!(!package.exists());
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn corrupt_cache_is_removed_and_valid_cache_is_rehashed() {
        let dir = unique_test_path("rehash", "dir");
        std::fs::create_dir(&dir).unwrap();
        let manifest = test_msi_manifest();
        let (_, package) = cache_paths(&dir, &manifest).unwrap();
        std::fs::write(&package, b"bad").unwrap();
        assert!(verified_cache(&package, &manifest.sha256).unwrap().is_none());
        assert!(!package.exists());
        std::fs::write(&package, b"abc").unwrap();
        let verified = verified_cache(&package, &manifest.sha256).unwrap().unwrap();
        #[cfg(windows)]
        assert!(std::fs::write(&package, b"bad").is_err());
        drop(verified);
        std::fs::remove_file(&package).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    // These closures substitute OS boundaries, while production run_with owns the
    // ordering/skip decisions and download_with performs the real hash/cache work.
    #[test]
    fn run_never_launches_after_download_checksum_failure() {
        let manifest = test_msi_manifest();
        let dir = unique_test_path("run-checksum", "dir");
        fs::create_dir(&dir).unwrap();
        let (part, package) = cache_paths(&dir, &manifest).unwrap();
        let result = run_with(
            &manifest,
            &|_, _| {},
            |_| Ok(false),
            || {
                download_with(&manifest, &part, &package, &|_, _| {}, |url| {
                    Ok(PackageResponse {
                        url: url.clone(), status: 200, location: None, length: None,
                        body: Cursor::new(b"corrupt"),
                    })
                })?;
                Ok(())
            },
            |_| panic!("checksum failure must not launch"),
        );
        assert!(matches!(result, Err(RemoteSoftwareError::ChecksumMismatch)));
        assert!(!part.exists());
        assert!(!package.exists());
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn run_download_only_verifies_without_detection_or_launch() {
        let mut manifest = test_msi_manifest();
        manifest.mode = InstallMode::DownloadOnly;
        let dir = unique_test_path("run-download", "dir");
        fs::create_dir(&dir).unwrap();
        let (part, package) = cache_paths(&dir, &manifest).unwrap();
        let result = run_with(
            &manifest,
            &|_, _| {},
            |_| panic!("download-only must not detect"),
            || {
                download_with(&manifest, &part, &package, &|_, _| {}, |url| {
                    Ok(PackageResponse {
                        url: url.clone(), status: 200, location: None, length: Some(3),
                        body: Cursor::new(b"abc"),
                    })
                })?;
                verified_cache(&package, &manifest.sha256)?.ok_or(RemoteSoftwareError::Cache)
            },
            |_| panic!("download-only must not launch"),
        ).unwrap();
        assert_eq!(result, (RemoteSoftwareStage::Success, None));
        assert_eq!(fs::read(&package).unwrap(), b"abc");
        fs::remove_file(package).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn run_already_installed_skips_cache_and_launch() {
        let result = run_with::<()>(
            &test_msi_manifest(), &|_, _| {}, |_| Ok(true),
            || panic!("already installed must not prepare cache"),
            |_| panic!("already installed must not launch"),
        ).unwrap();
        assert_eq!(result, (RemoteSoftwareStage::AlreadyInstalled, None));
    }

    #[test]
    fn run_rechecks_detection_and_retains_prepared_resource_until_exit() {
        use std::cell::{Cell, RefCell};
        struct Prepared<'a>(&'a Cell<bool>);
        impl Drop for Prepared<'_> {
            fn drop(&mut self) { self.0.set(true); }
        }
        for installed_after_download in [false, true] {
            let events = RefCell::new(Vec::new());
            let dropped = Cell::new(false);
            let mut detections = 0;
            let result = run_with(
                &test_msi_manifest(), &|_, _| {},
                |_| {
                    events.borrow_mut().push("detect");
                    detections += 1;
                    Ok(detections == 2 && installed_after_download)
                },
                || { events.borrow_mut().push("prepare"); Ok(Prepared(&dropped)) },
                |_| {
                    assert!(!dropped.get(), "verified resource released before launch");
                    events.borrow_mut().push("launch");
                    Ok(Some(3010))
                },
            ).unwrap();
            assert!(dropped.get());
            if installed_after_download {
                assert_eq!(*events.borrow(), ["detect", "prepare", "detect"]);
                assert_eq!(result, (RemoteSoftwareStage::AlreadyInstalled, None));
            } else {
                assert_eq!(*events.borrow(), ["detect", "prepare", "detect", "launch"]);
                assert_eq!(result, (RemoteSoftwareStage::NeedsReboot, Some(3010)));
            }
        }
    }

    #[test]
    fn run_rejects_manifest_controls_before_any_side_effect() {
        let mut manifest = test_msi_manifest();
        manifest.installer_type = InstallerType::Exe;
        manifest.package_url = "https://szxinyu.com/app.exe".into();
        manifest.silent_args = vec!["/D=bad\npath".into()];
        assert!(run_with::<()>(
            &manifest, &|_, _| {},
            |_| panic!("invalid manifest must not detect"),
            || panic!("invalid manifest must not prepare"),
            |_| panic!("invalid manifest must not launch"),
        ).is_err());
        manifest.silent_args = vec![r"/D=C:\Program Files\R&D\PowerShellTools^pwsh".into()];
        assert!(run_with(&manifest, &|_, _| {}, |_| Ok(false), || Ok(()), |_| Ok(Some(0))).is_ok());
    }

    #[test]
    fn registry_detection_consults_both_views_for_both_rule_types() {
        for rule in [test_msi_manifest().detection_rule, DetectionRule::UninstallDisplayName("Example".into())] {
            for installed in [None, Some(RegistryView::Registry32), Some(RegistryView::Registry64)] {
                let mut consulted = Vec::new();
                let found = detect_registry_views(&rule, |view, received_rule| {
                    assert_eq!(received_rule, &rule);
                    consulted.push(view);
                    Ok(Some(view) == installed)
                }).unwrap();
                assert_eq!(found, installed.is_some());
                let expected = if installed == Some(RegistryView::Registry64) {
                    vec![RegistryView::Registry64]
                } else { vec![RegistryView::Registry64, RegistryView::Registry32] };
                assert_eq!(consulted, expected);
            }
        }
        assert!(detect_registry_views(&test_msi_manifest().detection_rule, |_, _| {
            Err(RemoteSoftwareError::Detection)
        }).is_err());
    }

    #[test]
    fn transport_checks_initial_redirect_and_final_urls_before_writing() {
        let mut manifest = test_msi_manifest();
        let dir = unique_test_path("transport", "dir");
        fs::create_dir(&dir).unwrap();
        let (part, package) = cache_paths(&dir, &manifest).unwrap();
        manifest.package_url = "https://evil-szxinyu.com/app.msi".into();
        assert!(download_with::<Cursor<Vec<u8>>>(
            &manifest, &part, &package, &|_, _| {},
            |_| panic!("invalid initial URL must not reach transport"),
        ).is_err());
        manifest = test_msi_manifest();
        for bad_final in [false, true] {
            let mut calls = 0;
            assert!(download_with(&manifest, &part, &package, &|_, _| {}, |url| {
                calls += 1;
                assert_eq!(calls, 1, "invalid redirect must not be requested");
                Ok(PackageResponse {
                    url: if bad_final { url::Url::parse("https://evil.example/app.msi").unwrap() } else { url.clone() },
                    status: if bad_final { 200 } else { 302 },
                    location: Some("https://evil.example/app.msi".into()), length: Some(3),
                    body: Cursor::new(b"abc"),
                })
            }).is_err());
            assert!(!part.exists());
            assert!(!package.exists());
        }
        let mut calls = 0;
        assert!(download_with(&manifest, &part, &package, &|_, _| {}, |url| {
            calls += 1;
            Ok(PackageResponse {
                url: url.clone(), status: 302, location: Some("/again.msi".into()),
                length: None, body: Cursor::new(b""),
            })
        }).is_err());
        assert_eq!(calls, 6, "initial request plus five redirects");
        assert!(!part.exists());
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn transport_follows_approved_redirect_and_rejects_oversized_headers() {
        let manifest = test_msi_manifest();
        let dir = unique_test_path("approved-transport", "dir");
        fs::create_dir(&dir).unwrap();
        let (part, package) = cache_paths(&dir, &manifest).unwrap();
        let mut requested = Vec::new();
        download_with(&manifest, &part, &package, &|_, _| {}, |url| {
            requested.push(url.as_str().to_owned());
            let redirect = requested.len() == 1;
            Ok(PackageResponse {
                url: url.clone(), status: if redirect { 302 } else { 200 },
                location: redirect.then(|| "https://szxinyu.com/final.msi".into()),
                length: Some(3), body: Cursor::new(b"abc"),
            })
        }).unwrap();
        assert_eq!(requested, ["https://update.szxinyu.com/office.msi", "https://szxinyu.com/final.msi"]);
        assert_eq!(fs::read(&package).unwrap(), b"abc");
        fs::remove_file(&package).unwrap();
        let oversized = download_with(&manifest, &part, &package, &|_, _| {}, |url| {
            Ok(PackageResponse {
                url: url.clone(), status: 200, location: None, length: Some(2_147_483_649),
                body: InterruptedReader,
            })
        });
        assert!(matches!(oversized, Err(RemoteSoftwareError::DownloadFailed("package exceeds 2 GiB"))));
        assert!(!part.exists());
        assert!(!package.exists());
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn cache_policy_rejects_reparse_points_and_untrusted_owners() {
        assert!(reject_reparse_attributes(0x10).is_ok());
        assert!(reject_reparse_attributes(0x410).is_err());
        assert!(reject_reparse_attributes(0x400).is_err());
        assert!(check_trusted_owner(true, false).is_ok());
        assert!(check_trusted_owner(false, true).is_ok());
        assert!(check_trusted_owner(false, false).is_err());
        assert!(run_with::<()>(
            &test_msi_manifest(), &|_, _| {}, |_| Ok(false),
            || { reject_reparse_attributes(0x410)?; Ok(()) },
            |_| panic!("unsafe cache must not launch"),
        ).is_err());
    }

    fn test_state_entry(root: &Path) -> RecoveryEntry {
        let manifest = test_msi_manifest();
        RecoveryEntry {
            verified_package_path: root.join("packages").join(
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad.msi",
            ),
            last_result: Some(RemoteSoftwareStatus {
                request_id: manifest.request_id.clone(),
                stage: RemoteSoftwareStage::Failed,
                message: "installer_failed".into(),
                exit_code: Some(1603),
                needs_reboot: false,
                progress_percent: None,
            }),
            attempt_timestamps: vec![1_789_000_000, 1_789_000_300],
            failure_count: 2,
            paused: true,
            manifest,
        }
    }

    fn test_state_store(label: &str) -> (PathBuf, StateStore) {
        let root = unique_test_path(label, "dir");
        fs::create_dir_all(root.join("packages")).unwrap();
        let store = StateStore::with_root(root.clone());
        (root, store)
    }

    #[test]
    fn recovery_state_round_trips_complete_metadata() {
        let (root, store) = test_state_store("state-round-trip");
        let expected = RecoveryState {
            entries: vec![test_state_entry(&root)],
        };
        store.save(&expected).unwrap();
        assert_eq!(store.load().unwrap(), expected);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_state_load_discards_malformed_and_untrusted_entries() {
        let (root, store) = test_state_store("state-validation");
        fs::write(store.state_path(), b"{interrupted").unwrap();
        assert!(store.load().unwrap().entries.is_empty());

        let valid = test_state_entry(&root);
        let mut invalid_manifest = valid.clone();
        invalid_manifest.manifest.package_url = "https://evil.example/app.msi".into();
        let mut outside_cache = valid.clone();
        outside_cache.verified_package_path = root.join("outside.msi");
        let mut unsupported_name = valid.clone();
        unsupported_name.verified_package_path = root.join("packages").join("setup.zip");
        let persisted = RecoveryState {
            entries: vec![valid.clone(), invalid_manifest, outside_cache, unsupported_name],
        };
        fs::write(store.state_path(), serde_json::to_vec(&persisted).unwrap()).unwrap();
        assert_eq!(store.load().unwrap().entries, vec![valid]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_state_load_keeps_valid_sibling_of_structurally_invalid_entry() {
        let (root, store) = test_state_store("state-structural-validation");
        let valid = test_state_entry(&root);
        let persisted = serde_json::json!({
            "entries": [
                { "manifest": 42, "failure_count": "not-a-number" },
                serde_json::to_value(&valid).unwrap()
            ]
        });
        fs::write(store.state_path(), serde_json::to_vec(&persisted).unwrap()).unwrap();
        assert_eq!(store.load().unwrap().entries, vec![valid]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_save_atomically_replaces_old_file_without_temporary_residue() {
        let (root, store) = test_state_store("state-atomic-replace");
        let first = RecoveryState {
            entries: vec![test_state_entry(&root)],
        };
        store.save(&first).unwrap();
        let mut replacement_entry = test_state_entry(&root);
        replacement_entry.failure_count = 0;
        replacement_entry.paused = false;
        let replacement = RecoveryState {
            entries: vec![replacement_entry],
        };
        store.save(&replacement).unwrap();
        assert_eq!(store.load().unwrap(), replacement);
        let names = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names.iter().filter(|name| name.ends_with(".tmp")).count(), 0);
        assert!(names.iter().any(|name| name == "state.json"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_partial_cleanup_is_direct_and_cache_scoped() {
        let (root, store) = test_state_store("state-part-cleanup");
        let packages = root.join("packages");
        let stale = packages.join("download.part");
        let verified = packages.join(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad.msi",
        );
        let unrelated = packages.join("keep.txt");
        let nested = packages.join("nested");
        let nested_part = nested.join("keep.part");
        let outside_part = root.join("outside.part");
        fs::create_dir(&nested).unwrap();
        for path in [&stale, &verified, &unrelated, &nested_part, &outside_part] {
            fs::write(path, b"keep-or-remove").unwrap();
        }
        store.remove_stale_part_files().unwrap();
        assert!(!stale.exists());
        for path in [&verified, &unrelated, &nested_part, &outside_part] {
            assert!(path.exists(), "cleanup escaped its direct .part scope: {path:?}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_partial_cleanup_rejects_direct_reparse_before_deletion() {
        let (root, store) = test_state_store("state-reparse-part");
        let tagged = root.join("packages").join("tagged.part");
        fs::write(&tagged, b"must-remain").unwrap();
        let result = store.remove_stale_part_files_with(|path, _| {
            if path == tagged {
                reject_reparse_attributes(0x400)
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&tagged).unwrap(), b"must-remain");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_state_is_inert_and_preserves_verified_package() {
        let (root, store) = test_state_store("state-inert-load");
        let verified = test_state_entry(&root).verified_package_path;
        fs::write(&verified, b"verified-package").unwrap();
        let mut invalid = test_state_entry(&root);
        invalid.manifest.sha256 = "not-a-digest".into();
        fs::write(
            store.state_path(),
            serde_json::to_vec(&RecoveryState {
                entries: vec![invalid],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(store.load().unwrap().entries.is_empty());
        assert_eq!(fs::read(&verified).unwrap(), b"verified-package");
        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Clone)]
    struct TestWorkerStore {
        root: PathBuf,
        state: RecoveryState,
        events: Rc<RefCell<Vec<&'static str>>>,
        saved: Rc<RefCell<Vec<RecoveryState>>>,
        stop_on_queued_save: Option<Rc<WorkerStop>>,
    }

    impl WorkerStateStore for TestWorkerStore {
        fn remove_stale_part_files(&mut self) -> Result<(), RemoteSoftwareError> {
            self.events.borrow_mut().push("cleanup");
            Ok(())
        }

        fn load(&mut self) -> Result<RecoveryState, RemoteSoftwareError> {
            self.events.borrow_mut().push("load");
            Ok(self.state.clone())
        }

        fn save(&mut self, state: &RecoveryState) -> Result<(), RemoteSoftwareError> {
            self.events.borrow_mut().push("save");
            self.state = state.clone();
            self.saved.borrow_mut().push(state.clone());
            let queued = state.entries.iter().any(|entry| {
                entry
                    .last_result
                    .as_ref()
                    .map(|status| status.stage == RemoteSoftwareStage::Queued)
                    .unwrap_or(false)
            });
            if queued {
                if let Some(stop) = &self.stop_on_queued_save {
                    stop.stop();
                }
            }
            Ok(())
        }

        fn package_root(&self) -> PathBuf {
            self.root.join("packages")
        }
    }

    fn test_worker_store(label: &str) -> TestWorkerStore {
        let root = unique_test_path(label, "dir");
        let events = Rc::new(RefCell::new(Vec::new()));
        let saved = Rc::new(RefCell::new(Vec::new()));
        let mut entry = test_state_entry(&root);
        entry.last_result = None;
        entry.attempt_timestamps.clear();
        entry.failure_count = 0;
        entry.paused = false;
        TestWorkerStore {
            state: RecoveryState {
                entries: vec![entry],
            },
            root,
            events,
            saved,
            stop_on_queued_save: None,
        }
    }

    fn failed_worker_status(request_id: &str) -> RemoteSoftwareStatus {
        RemoteSoftwareStatus {
            request_id: request_id.into(),
            stage: RemoteSoftwareStage::Failed,
            message: "installer_busy".into(),
            exit_code: None,
            needs_reboot: false,
            progress_percent: None,
        }
    }

    #[test]
    fn worker_startup_orders_cleanup_load_then_runs_five_minute_checks() {
        let mut store = test_worker_store("worker-startup-order");
        let events = store.events.clone();
        let waits = Rc::new(RefCell::new(Vec::new()));
        let waits_seen = waits.clone();
        let mut detections = 0;
        run_worker_loop(
            &mut store,
            &WorkerStop::default(),
            || 1_789_000_000,
            move |duration, _| {
                waits_seen.borrow_mut().push(duration);
                waits_seen.borrow().len() == 1
            },
            |_| {
                detections += 1;
                events.borrow_mut().push("detect");
                Ok(true)
            },
            |_| panic!("installed entry must not execute"),
        )
        .unwrap();
        assert_eq!(&store.events.borrow()[..3], ["cleanup", "load", "detect"]);
        assert_eq!(detections, 2);
        assert_eq!(
            *waits.borrow(),
            [Duration::from_secs(5 * 60), Duration::from_secs(5 * 60)]
        );
    }

    #[test]
    fn worker_executes_only_missing_entries_and_resets_success() {
        let mut store = test_worker_store("worker-missing-execute");
        let request_id = store.state.entries[0].manifest.request_id.clone();
        let mut executions = 0;
        run_worker_loop(
            &mut store,
            &WorkerStop::default(),
            || 1_789_000_000,
            |_, _| false,
            |_| Ok(false),
            |_| {
                executions += 1;
                Some(RemoteSoftwareStatus {
                    request_id: request_id.clone(),
                    stage: RemoteSoftwareStage::Success,
                    message: String::new(),
                    exit_code: Some(0),
                    needs_reboot: false,
                    progress_percent: Some(100),
                })
            },
        )
        .unwrap();
        assert_eq!(executions, 1);
        let entry = &store.state.entries[0];
        assert_eq!(entry.failure_count, 0);
        assert!(entry.attempt_timestamps.is_empty());
        assert!(!entry.paused);
        assert_eq!(entry.last_result.as_ref().unwrap().stage, RemoteSoftwareStage::Success);
    }

    #[test]
    fn retry_schedule_applies_backoff_hourly_limit_and_pause_until_restart() {
        let root = unique_test_path("worker-retry-policy", "dir");
        let mut entry = test_state_entry(&root);
        entry.attempt_timestamps.clear();
        entry.failure_count = 0;
        entry.paused = false;
        for (now, next) in [(0, 300), (300, 1_200)] {
            assert_eq!(attempt_decision(&entry, now), AttemptDecision::Ready);
            record_attempt_started(&mut entry, now);
            let request_id = entry.manifest.request_id.clone();
            apply_execution_result(
                &mut entry,
                failed_worker_status(&request_id),
                now,
            );
            assert_eq!(attempt_decision(&entry, now), AttemptDecision::WaitUntil(next));
        }
        assert_eq!(attempt_decision(&entry, 1_200), AttemptDecision::Ready);
        record_attempt_started(&mut entry, 1_200);
        let request_id = entry.manifest.request_id.clone();
        apply_execution_result(
            &mut entry,
            failed_worker_status(&request_id),
            1_200,
        );
        assert!(entry.paused);
        assert_eq!(attempt_decision(&entry, 3_000), AttemptDecision::Paused);
        assert_eq!(attempt_decision(&entry, 3_600), AttemptDecision::Paused);
        reset_paused_for_restart(&mut entry);
        assert!(!entry.paused);
        assert_eq!(entry.failure_count, 0);
        assert!(entry.attempt_timestamps.is_empty());
        assert_eq!(attempt_decision(&entry, 3_600), AttemptDecision::Ready);
    }

    #[test]
    fn retry_delay_index_follows_completed_failure_count() {
        let root = unique_test_path("worker-retry-index", "dir");
        let mut entry = test_state_entry(&root);
        entry.paused = false;
        for (failure_count, last_attempt, expected_due) in
            [(1, 0, 300), (2, 300, 1_200), (3, 1_200, 3_000)]
        {
            entry.failure_count = failure_count;
            entry.attempt_timestamps = vec![last_attempt];
            assert_eq!(
                attempt_decision(&entry, last_attempt),
                AttemptDecision::WaitUntil(expected_due)
            );
        }
    }

    #[test]
    fn installed_detection_records_status_and_clears_failure_window() {
        let mut store = test_worker_store("worker-installed-reset");
        run_worker_loop(
            &mut store,
            &WorkerStop::default(),
            || 1_789_000_000,
            |_, _| false,
            |_| Ok(true),
            |_| panic!("installed entry must not execute"),
        )
        .unwrap();
        let entry = &store.state.entries[0];
        assert_eq!(entry.failure_count, 0);
        assert!(entry.attempt_timestamps.is_empty());
        assert!(!entry.paused);
        let status = entry.last_result.as_ref().unwrap();
        assert_eq!(status.stage, RemoteSoftwareStage::AlreadyInstalled);
        assert_eq!(status.message, "already_installed");
    }

    #[test]
    fn stopped_worker_exits_before_cleanup_or_execution() {
        let stop = WorkerStop::default();
        stop.stop();
        assert!(!stop.wait(Duration::from_secs(5 * 60)));
        let mut store = test_worker_store("worker-stopped");
        run_worker_loop(
            &mut store,
            &stop,
            || 1_789_000_000,
            |_, _| panic!("stopped worker must not wait"),
            |_| panic!("stopped worker must not detect"),
            |_| panic!("stopped worker must not execute"),
        )
        .unwrap();
        assert!(store.events.borrow().is_empty());
    }

    #[test]
    fn stop_after_detection_prevents_a_new_install_attempt() {
        let stop = WorkerStop::default();
        let mut store = test_worker_store("worker-stop-before-execute");
        run_worker_loop(
            &mut store,
            &stop,
            || 1_789_000_000,
            |_, _| false,
            |_| {
                stop.stop();
                Ok(false)
            },
            |_| panic!("stop after detection must prevent execute"),
        )
        .unwrap();
        assert_eq!(store.state.entries[0].failure_count, 0);
        assert!(store.state.entries[0].attempt_timestamps.is_empty());
    }

    #[test]
    fn stop_after_queued_state_save_blocks_execution_gate() {
        let stop = Rc::new(WorkerStop::default());
        let mut store = test_worker_store("worker-stop-after-save");
        store.stop_on_queued_save = Some(stop.clone());
        let mut executions = 0;
        run_worker_loop(
            &mut store,
            &stop,
            || 1_789_000_000,
            |_, _| false,
            |_| Ok(false),
            |_| {
                executions += 1;
                Some(failed_worker_status("must-not-run"))
            },
        )
        .unwrap();
        assert_eq!(executions, 0);
        assert!(stop.is_stopped());
        assert_eq!(store.state.entries[0].failure_count, 1);
        assert_eq!(
            store.state.entries[0].last_result.as_ref().unwrap().stage,
            RemoteSoftwareStage::Queued
        );
    }

    #[test]
    fn worker_entry_gate_requires_windows_installed_exact_service_process() {
        let service = vec![OsString::from("--service")];
        assert!(worker_entry_allowed(true, true, true, &service));
        for (windows, installed, available, args) in [
            (false, true, true, service.clone()),
            (true, false, true, service.clone()),
            (true, true, false, service.clone()),
            (true, true, true, Vec::new()),
            (true, true, true, vec![OsString::from("--portable-service")]),
            (true, true, true, vec![OsString::from("--service"), OsString::from("--server")]),
        ] {
            assert!(!worker_entry_allowed(windows, installed, available, &args));
        }
    }

    #[test]
    fn invalid_loaded_entry_is_removed_before_detection_or_execution() {
        let mut store = test_worker_store("worker-invalid-entry");
        store.state.entries[0].verified_package_path = store.root.join("outside.exe");
        run_worker_loop(
            &mut store,
            &WorkerStop::default(),
            || 1_789_000_000,
            |_, _| false,
            |_| panic!("invalid entry must not detect"),
            |_| panic!("invalid entry must not execute"),
        )
        .unwrap();
        assert!(store.state.entries.is_empty());
        assert_eq!(store.saved.borrow().last().unwrap().entries.len(), 0);
    }

    #[test]
    fn invalid_detection_rule_is_removed_before_detector_or_executor() {
        let mut store = test_worker_store("worker-invalid-detection-rule");
        store.state.entries[0].manifest.detection_rule =
            DetectionRule::MsiProductCode("not-a-product-code".into());
        run_worker_loop(
            &mut store,
            &WorkerStop::default(),
            || 1_789_000_000,
            |_, _| false,
            |_| panic!("invalid detection rule must not reach detector"),
            |_| panic!("invalid detection rule must not reach executor"),
        )
        .unwrap();
        assert!(store.state.entries.is_empty());
        assert_eq!(store.saved.borrow().last().unwrap().entries.len(), 0);
    }

    #[test]
    fn bounded_stop_does_not_join_blocked_executor_or_start_next_attempt() {
        let stop = Arc::new(WorkerStop::default());
        let completion = Arc::new(WorkerCompletion::default());
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker_stop = stop.clone();
        let worker_completion = completion.clone();
        let worker_started = started.clone();
        let worker_release = release.clone();
        let worker_attempts = attempts.clone();
        let thread = std::thread::spawn(move || {
            let _completion = WorkerCompletionGuard {
                completion: worker_completion,
            };
            worker_attempts.fetch_add(1, Ordering::SeqCst);
            let (ready, wake) = &*worker_started;
            *ready.lock().unwrap() = true;
            wake.notify_all();
            let (released, wake) = &*worker_release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            if !worker_stop.is_stopped() {
                worker_attempts.fetch_add(1, Ordering::SeqCst);
            }
        });
        let mut handle = SelfHealWorkerHandle {
            stop,
            completion: completion.clone(),
            thread: Some(thread),
        };
        let (ready, wake) = &*started;
        let mut ready = ready.lock().unwrap();
        while !*ready {
            ready = wake.wait(ready).unwrap();
        }
        drop(ready);
        let started_at = Instant::now();
        handle.stop_inner(Duration::from_millis(20));
        assert!(started_at.elapsed() < Duration::from_millis(500));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        let (released, wake) = &*release;
        *released.lock().unwrap() = true;
        wake.notify_all();
        assert!(completion.wait(Duration::from_secs(1)));
    }

    #[test]
    fn missing_terminal_observer_status_keeps_state_and_verified_package() {
        let (root, mut store) = test_state_store("worker-observer-disappeared");
        let mut entry = test_state_entry(&root);
        entry.attempt_timestamps.clear();
        entry.failure_count = 0;
        entry.paused = false;
        let verified = entry.verified_package_path.clone();
        fs::write(&verified, b"verified-package").unwrap();
        store
            .save(&RecoveryState {
                entries: vec![entry],
            })
            .unwrap();
        run_worker_loop(
            &mut store,
            &WorkerStop::default(),
            || 1_789_000_000,
            |_, _| false,
            |_| Ok(false),
            |_| None,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].failure_count, 1);
        assert_eq!(loaded.entries[0].last_result.as_ref().unwrap().stage, RemoteSoftwareStage::Failed);
        assert_eq!(fs::read(&verified).unwrap(), b"verified-package");
        fs::remove_dir_all(root).unwrap();
    }
}
