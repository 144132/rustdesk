//! One operation only. Authorization, persistence and recovery scheduling belong to callers.
use crate::remote_software::{
    installer_outcome, validate_manifest, validate_package_url, validate_sha256, InstallOutcome,
    InstallerType, RemoteSoftwareManifest,
};
use hbb_common::thiserror;
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
}

const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

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
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(RemoteSoftwareError::Cache);
        }
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
            Storage::FileSystem::CreateDirectoryW,
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
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let uninstall = match machine.open_subkey_with_flags(UNINSTALL_KEY, KEY_READ | view) {
                Ok(key) => key,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(RemoteSoftwareError::Detection),
            };
            if let DetectionRule::MsiProductCode(code) = rule {
                match uninstall.open_subkey_with_flags(code, KEY_READ | view) {
                    Ok(_) => return Ok(true),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
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
        }
        Ok(false)
    }

    fn validate_exe_args(args: &[String]) -> Result<(), RemoteSoftwareError> {
        for argument in args {
            let lower = argument.to_ascii_lowercase();
            if argument.contains(['&', '|', '<', '>', '^', '\r', '\n', '\0'])
                || ["cmd.exe", "powershell", "pwsh", "wscript", "cscript", "mshta", "-encodedcommand"]
                    .iter().any(|token| lower.contains(token))
            {
                return Err(RemoteSoftwareError::InvalidManifest("unsupported EXE argument"));
            }
        }
        Ok(())
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
                validate_exe_args(&manifest.silent_args)?;
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
        if owner.0.is_null() || !unsafe {
            IsWellKnownSid(owner, WinLocalSystemSid).as_bool()
                || IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool()
        } { return Err(RemoteSoftwareError::Cache); }
        Ok(())
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
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"), 1, &mut descriptor, None)
        }.map_err(|_| RemoteSoftwareError::Cache)?;
        let descriptor = LocalDescriptor(descriptor);
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
        let mut url = validate_package_url(&manifest.package_url)?;
        let mut followed = 0;
        loop {
            validate_package_url(url.as_str())?;
            let response = client.get(url.clone()).send()
                .map_err(|_| RemoteSoftwareError::DownloadFailed("request failed"))?;
            validate_package_url(response.url().as_str())?;
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                let location = response.headers().get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(RemoteSoftwareError::DownloadFailed("redirect missing Location"))?;
                url = checked_redirect(response.url(), location, followed)?;
                followed += 1;
                continue;
            }
            if response.status() != reqwest::StatusCode::OK {
                return Err(RemoteSoftwareError::DownloadFailed("HTTP status rejected"));
            }
            let length = response.content_length();
            if let Some(length) = length { checked_size(0, length)?; }
            let last = std::cell::Cell::new(None);
            write_verified(response, part, package, &manifest.sha256, |bytes| {
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

    fn run(
        manifest: &RemoteSoftwareManifest,
        progress: &impl Fn(RemoteSoftwareStage, Option<u8>),
    ) -> Result<(RemoteSoftwareStage, Option<i32>), RemoteSoftwareError> {
        validate_manifest(manifest)?;
        validate_detection(&manifest.detection_rule)?;
        // Fail malformed execution requests before any registry/network/cache work.
        match manifest.installer_type {
            InstallerType::Exe => validate_exe_args(&manifest.silent_args)?,
            InstallerType::Msi if !manifest.silent_args.is_empty() => {
                return Err(RemoteSoftwareError::InvalidManifest("MSI arguments are fixed"));
            }
            InstallerType::Msi => {},
        }
        if manifest.mode == InstallMode::DownloadAndInstall {
            progress(RemoteSoftwareStage::Detecting, None);
            if detect_installed(&manifest.detection_rule)? {
                return Ok((RemoteSoftwareStage::AlreadyInstalled, None));
            }
        }
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
        if manifest.mode == InstallMode::DownloadOnly {
            return Ok((RemoteSoftwareStage::Success, None));
        }
        // Recheck after a potentially long download; another installer may have completed.
        progress(RemoteSoftwareStage::Detecting, None);
        if detect_installed(&manifest.detection_rule)? {
            return Ok((RemoteSoftwareStage::AlreadyInstalled, None));
        }
        let mut command = build_process_command(manifest, &package)?;
        command.current_dir(&directory.path);
        progress(RemoteSoftwareStage::Installing, None);
        let exit = command.status().map_err(|_| RemoteSoftwareError::Installer)?;
        drop(verified);
        let stage = match installer_outcome(exit.code()) {
            InstallOutcome::Success => RemoteSoftwareStage::Success,
            InstallOutcome::NeedsReboot => RemoteSoftwareStage::NeedsReboot,
            InstallOutcome::Failed => RemoteSoftwareStage::Failed,
        };
        Ok((stage, exit.code()))
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
    use crate::remote_software::{DetectionRule, InstallMode, InstallerType};
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
    fn exe_command_preserves_argument_boundaries_and_rejects_shell_forms() {
        let mut manifest = test_msi_manifest();
        manifest.installer_type = InstallerType::Exe;
        manifest.package_url = "https://szxinyu.com/app.exe".into();
        manifest.silent_args = vec!["/S".into(), r"/D=C:\Program Files\Example".into()];
        let command = build_process_command(&manifest, Path::new(r"C:\pkg\app.exe")).unwrap();
        assert_eq!(command.get_program(), Path::new(r"C:\pkg\app.exe"));
        assert_eq!(command.get_args().collect::<Vec<_>>(), manifest.silent_args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>());
        for argument in ["/S & whoami", "powershell.exe", "-EncodedCommand", "a|b", ">out"] {
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
}
