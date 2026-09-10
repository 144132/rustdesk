//! One operation only. Authorization, persistence and recovery scheduling belong to callers.
use crate::remote_software::{
    installer_outcome, validate_manifest, validate_package_url, validate_sha256, InstallOutcome,
    DetectionRule, InstallMode, InstallerType, RemoteSoftwareManifest, RemoteSoftwareStage,
    RemoteSoftwareStatus,
};
use hbb_common::thiserror;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct RecoveryState {
    pub entries: Vec<RecoveryEntry>,
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

#[derive(Clone, Debug)]
pub struct StateStore {
    root: PathBuf,
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
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    #[cfg(windows)]
    pub fn production() -> Result<Self, RemoteSoftwareError> {
        Ok(Self::with_root(native::prepare_state_root()?))
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
        let mut state: RecoveryState = match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(_) => return Ok(RecoveryState::default()),
        };
        state.entries.retain(|entry| self.validate_entry(entry).is_ok());
        Ok(state)
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
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".part"))
            {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(|_| RemoteSoftwareError::State)?;
            if metadata.is_file() && !metadata.file_type().is_symlink() {
                fs::remove_file(entry.path()).map_err(|_| RemoteSoftwareError::State)?;
            }
        }
        Ok(())
    }

    fn validate_entry(&self, entry: &RecoveryEntry) -> Result<(), RemoteSoftwareError> {
        validate_manifest(&entry.manifest)?;
        let (_, expected) = cache_paths(&self.package_root(), &entry.manifest)?;
        if entry.verified_package_path != expected {
            return Err(RemoteSoftwareError::State);
        }
        Ok(())
    }
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

    fn validate_detection(rule: &DetectionRule) -> Result<(), RemoteSoftwareError> {
        // Task 1 keeps its path validator private; use its public manifest boundary.
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
            if bytes.len() != 38 || bytes[0] != b'{' || bytes[37] != b'}'
                || bytes[1..37].iter().enumerate().any(|(index, byte)| {
                    if [8, 13, 18, 23].contains(&index) { *byte != b'-' } else { !byte.is_ascii_hexdigit() }
                })
            {
                return Err(RemoteSoftwareError::InvalidManifest("invalid MSI ProductCode"));
            }
        }
        Ok(())
    }

    pub fn detect_installed(rule: &DetectionRule) -> Result<bool, RemoteSoftwareError> {
        validate_detection(rule)?;
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

    struct CacheDirectory {
        path: PathBuf,
        // Deny directory rename/deletion for the entire operation, including installation.
        _handles: Vec<File>,
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

    fn prepare_cache() -> Result<CacheDirectory, RemoteSoftwareError> {
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

    pub(super) fn prepare_state_root() -> Result<PathBuf, RemoteSoftwareError> {
        let directory = prepare_cache()?;
        directory
            .path
            .parent()
            .map(Path::to_path_buf)
            .ok_or(RemoteSoftwareError::State)
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
        validate_detection(&manifest.detection_rule)?;
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
    use std::io::{self, Cursor, Read};
    use std::path::{Path, PathBuf};

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
}
