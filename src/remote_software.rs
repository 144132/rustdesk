use hbb_common::thiserror;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};
use url::Url;

const ALLOWED_HOST: &str = "szxinyu.com";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallerType {
    #[serde(rename = "msi")]
    Msi,
    #[serde(rename = "exe")]
    Exe,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetectionRule {
    #[serde(rename = "msi_product_code")]
    MsiProductCode(String),
    #[serde(rename = "uninstall_display_name")]
    UninstallDisplayName(String),
    #[serde(rename = "exe_path")]
    ExePath(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallMode {
    #[serde(rename = "download_only")]
    DownloadOnly,
    #[serde(rename = "download_and_install")]
    DownloadAndInstall,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSoftwareManifest {
    pub request_id: String,
    pub software_name: String,
    pub package_url: String,
    pub sha256: String,
    pub installer_type: InstallerType,
    pub detection_rule: DetectionRule,
    pub silent_args: Vec<String>,
    pub mode: InstallMode,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RemoteSoftwareError {
    #[error("invalid package URL: {0}")]
    InvalidPackageUrl(String),
    #[error("invalid SHA-256 digest")]
    InvalidSha256,
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteSoftwareStage {
    Queued,
    Downloading,
    Verifying,
    Detecting,
    Installing,
    AlreadyInstalled,
    Success,
    NeedsReboot,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSoftwareStatus {
    pub request_id: String,
    pub stage: RemoteSoftwareStage,
    pub message: String,
    pub exit_code: Option<i32>,
    pub needs_reboot: bool,
    pub progress_percent: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Success,
    NeedsReboot,
    Failed,
}

pub fn validate_package_url(value: &str) -> Result<Url, RemoteSoftwareError> {
    if has_control_characters(value) {
        return Err(RemoteSoftwareError::InvalidPackageUrl(
            "URL must not contain control characters".into(),
        ));
    }
    let url = Url::parse(value)
        .map_err(|error| RemoteSoftwareError::InvalidPackageUrl(error.to_string()))?;
    let host = match url.host() {
        Some(url::Host::Domain(host)) => host,
        _ => {
            return Err(RemoteSoftwareError::InvalidPackageUrl(
                "host must be a domain name".into(),
            ))
        }
    };

    if !url.scheme().eq_ignore_ascii_case("https")
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || !(host.eq_ignore_ascii_case(ALLOWED_HOST)
            || host
                .to_ascii_lowercase()
                .strip_suffix(ALLOWED_HOST)
                .is_some_and(|prefix| prefix.ends_with('.')))
    {
        return Err(RemoteSoftwareError::InvalidPackageUrl(
            "URL is outside the HTTPS package allowlist".into(),
        ));
    }

    let path = Path::new(url.path());
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(RemoteSoftwareError::InvalidPackageUrl(
            "package path must name an installer".into(),
        ));
    };
    if file_name.is_empty() || has_control_characters(file_name) {
        return Err(RemoteSoftwareError::InvalidPackageUrl(
            "package name is invalid".into(),
        ));
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    if !matches!(extension.as_deref(), Some("msi" | "exe")) {
        return Err(RemoteSoftwareError::InvalidPackageUrl(
            "package extension must be .msi or .exe".into(),
        ));
    }
    Ok(url)
}

pub fn validate_sha256(value: &str) -> Result<[u8; 32], RemoteSoftwareError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RemoteSoftwareError::InvalidSha256);
    }
    let mut digest = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = (hex_value(chunk[0]) << 4) | hex_value(chunk[1]);
    }
    Ok(digest)
}

pub fn validate_manifest(manifest: &RemoteSoftwareManifest) -> Result<(), RemoteSoftwareError> {
    validate_non_empty_field(&manifest.request_id, "request_id")?;
    validate_non_empty_field(&manifest.software_name, "software_name")?;
    let url = validate_package_url(&manifest.package_url)
        .map_err(|error| RemoteSoftwareError::InvalidManifest(error.to_string()))?;
    validate_sha256(&manifest.sha256)?;

    let extension = Path::new(url.path())
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    let matches_type = matches!(
        (&manifest.installer_type, extension.as_deref()),
        (InstallerType::Msi, Some("msi")) | (InstallerType::Exe, Some("exe"))
    );
    if !matches_type {
        return Err(RemoteSoftwareError::InvalidManifest(
            "installer type must match package extension".into(),
        ));
    }

    match &manifest.detection_rule {
        DetectionRule::MsiProductCode(value) | DetectionRule::UninstallDisplayName(value) => {
            validate_non_empty_field(value, "detection value")?;
        }
        DetectionRule::ExePath(value) => {
            validate_non_empty_field(value, "exe path")?;
            if !is_local_windows_absolute_path(value) {
                return Err(RemoteSoftwareError::InvalidManifest(
                    "exe path must be absolute".into(),
                ));
            }
        }
    }
    if manifest
        .silent_args
        .iter()
        .any(|argument| has_control_characters(argument))
    {
        return Err(RemoteSoftwareError::InvalidManifest(
            "silent arguments must not contain control characters".into(),
        ));
    }
    Ok(())
}

pub fn installer_outcome(exit_code: Option<i32>) -> InstallOutcome {
    match exit_code {
        Some(0) => InstallOutcome::Success,
        Some(3010) => InstallOutcome::NeedsReboot,
        _ => InstallOutcome::Failed,
    }
}

pub fn retry_delay(attempt: u32) -> Option<Duration> {
    match attempt {
        0 => Some(Duration::from_secs(5 * 60)),
        1 => Some(Duration::from_secs(15 * 60)),
        2 => Some(Duration::from_secs(30 * 60)),
        _ => None,
    }
}

impl RemoteSoftwareStatus {
    pub fn success(request_id: &str, outcome: InstallOutcome) -> Self {
        let needs_reboot = outcome == InstallOutcome::NeedsReboot;
        let stage = match outcome {
            InstallOutcome::Success => RemoteSoftwareStage::Success,
            InstallOutcome::NeedsReboot => RemoteSoftwareStage::NeedsReboot,
            InstallOutcome::Failed => RemoteSoftwareStage::Failed,
        };
        Self {
            request_id: request_id.to_owned(),
            stage,
            message: String::new(),
            exit_code: None,
            needs_reboot,
            progress_percent: (outcome != InstallOutcome::Failed).then_some(100),
        }
    }
}

fn has_control_characters(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn is_local_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn validate_non_empty_field(value: &str, field: &str) -> Result<(), RemoteSoftwareError> {
    if value.trim().is_empty() || has_control_characters(value) {
        return Err(RemoteSoftwareError::InvalidManifest(format!(
            "{field} must be non-empty and contain no control characters"
        )));
    }
    Ok(())
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::{
        message_proto::{
            SoftwareDetectionType, SoftwareInstallAction, SoftwareInstallMode,
            SoftwareInstallRequest, SoftwareInstallStage, SoftwareInstallerType,
        },
        protobuf::Message as _,
    };
    use std::time::Duration;

    fn test_manifest(package_url: &str) -> RemoteSoftwareManifest {
        RemoteSoftwareManifest {
            request_id: "request-1".into(),
            software_name: "Example".into(),
            package_url: package_url.into(),
            sha256: "00".repeat(32),
            installer_type: InstallerType::Msi,
            detection_rule: DetectionRule::MsiProductCode("{PRODUCT-CODE}".into()),
            silent_args: Vec::new(),
            mode: InstallMode::DownloadAndInstall,
        }
    }

    fn test_install_request() -> SoftwareInstallRequest {
        SoftwareInstallRequest {
            request_id: "request-1".into(),
            software_name: "Example".into(),
            package_url: "https://update.szxinyu.com/app.exe".into(),
            sha256: "00".repeat(32),
            installer_type: SoftwareInstallerType::Exe.into(),
            detection_type: SoftwareDetectionType::ExePath.into(),
            detection_value: r"C:\Program Files\Example\Example.exe".into(),
            silent_args: vec!["/S".into()],
            mode: SoftwareInstallMode::DownloadAndInstall.into(),
            ..Default::default()
        }
    }

    #[test]
    fn software_install_request_round_trips_without_shell_command_fields() {
        let request = test_install_request();
        let mut action = SoftwareInstallAction::new();
        action.set_request(request.clone());
        let mut message = hbb_common::message_proto::Message::new();
        message.set_software_install_action(action);

        let bytes = message.write_to_bytes().unwrap();
        let decoded = hbb_common::message_proto::Message::parse_from_bytes(&bytes).unwrap();
        let decoded = decoded.software_install_action().request();
        assert_eq!(decoded.request_id, request.request_id);
        assert_eq!(decoded.silent_args, vec!["/S"]);
        assert!(decoded.silent_args.len() < 16);
    }

    #[test]
    fn legacy_features_do_not_advertise_software_install() {
        let mut current = hbb_common::message_proto::Features::new();
        assert!(!crate::client::software_install_feature_supported(None));
        assert!(!crate::client::software_install_feature_supported(Some(
            &current
        )));
        current.software_install = true;
        assert!(crate::client::software_install_feature_supported(Some(
            &current
        )));
    }

    #[test]
    fn software_install_stages_have_stable_flutter_names() {
        assert_eq!(
            crate::client::software_install_stage_name(SoftwareInstallStage::Queued.into()),
            "queued"
        );
        assert_eq!(
            crate::client::software_install_stage_name(
                SoftwareInstallStage::AlreadyInstalled.into()
            ),
            "already_installed"
        );
        assert_eq!(
            crate::client::software_install_stage_name(SoftwareInstallStage::NeedsReboot.into()),
            "needs_reboot"
        );
        assert_eq!(
            crate::client::software_install_stage_name(
                SoftwareInstallStage::UnknownInstallStage.into()
            ),
            "unknown"
        );
    }

    #[test]
    fn package_url_accepts_root_and_subdomains_but_rejects_lookalikes() {
        assert!(validate_package_url("https://szxinyu.com/a/app.msi").is_ok());
        assert!(validate_package_url("https://update.szxinyu.com/packages/app.exe").is_ok());
        assert!(validate_package_url("https://evil-szxinyu.com/app.exe").is_err());
        assert!(validate_package_url("http://update.szxinyu.com/app.exe").is_err());
        assert!(validate_package_url("https://update.szxinyu.com:8443/app.exe").is_err());
    }

    #[test]
    fn manifest_requires_sha256_and_matching_installer_extension() {
        let mut manifest = test_manifest("https://update.szxinyu.com/app.msi");
        manifest.sha256 = "".into();
        assert!(validate_manifest(&manifest).is_err());
        manifest.sha256 = "00".repeat(32);
        manifest.installer_type = InstallerType::Exe;
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn retry_delay_uses_bounded_backoff() {
        assert_eq!(retry_delay(0), Some(Duration::from_secs(5 * 60)));
        assert_eq!(retry_delay(1), Some(Duration::from_secs(15 * 60)));
        assert_eq!(retry_delay(2), Some(Duration::from_secs(30 * 60)));
        assert_eq!(retry_delay(3), None);
    }

    #[test]
    fn package_url_rejects_credentials_ips_and_invalid_paths() {
        for url in [
            "https://user:password@update.szxinyu.com/app.exe",
            "https://127.0.0.1/app.exe",
            "https://update.szxinyu.com/",
            "https://update.szxinyu.com/app.msi/extra",
        ] {
            assert!(validate_package_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn package_url_rejects_raw_control_characters() {
        for url in [
            "https://update.szxinyu.com/app\r.exe",
            "https://update.szxinyu.com/app\n.exe",
            "https://update.szxinyu.com/app\t.exe",
        ] {
            assert!(validate_package_url(url).is_err(), "{url:?}");
        }
    }

    #[test]
    fn manifest_rejects_invalid_fields_and_detection_paths() {
        let mut manifest = test_manifest("https://update.szxinyu.com/app.msi");
        manifest.software_name = "".into();
        assert!(validate_manifest(&manifest).is_err());
        manifest.software_name = "Example".into();
        manifest.detection_rule = DetectionRule::ExePath("relative\\app.exe".into());
        assert!(validate_manifest(&manifest).is_err());
        manifest.detection_rule = DetectionRule::MsiProductCode("".into());
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn exe_detection_accepts_only_local_drive_rooted_windows_paths() {
        let accepted = RemoteSoftwareManifest {
            package_url: "https://update.szxinyu.com/app.exe".into(),
            installer_type: InstallerType::Exe,
            detection_rule: DetectionRule::ExePath(
                r"C:\Program Files\App\App.exe".into(),
            ),
            ..test_manifest("https://update.szxinyu.com/app.msi")
        };
        assert!(validate_manifest(&accepted).is_ok());

        for path in [
            r"\\server\share\App.exe",
            r"C:Program Files\App\App.exe",
            r"Program Files\App\App.exe",
            "/tmp/App.exe",
        ] {
            let manifest = RemoteSoftwareManifest {
                package_url: "https://update.szxinyu.com/app.exe".into(),
                installer_type: InstallerType::Exe,
                detection_rule: DetectionRule::ExePath(path.into()),
                ..test_manifest("https://update.szxinyu.com/app.msi")
            };
            assert!(validate_manifest(&manifest).is_err(), "{path}");
        }
    }

    #[test]
    fn sha256_parser_requires_exact_hex_digest() {
        assert_eq!(validate_sha256(&"ab".repeat(32)).unwrap()[0], 0xab);
        assert!(validate_sha256("").is_err());
        assert!(validate_sha256(&"ab".repeat(31)).is_err());
        assert!(validate_sha256(&format!("{}g", "ab".repeat(31))).is_err());
    }

    #[test]
    fn installer_exit_codes_and_success_status_are_mapped() {
        assert_eq!(installer_outcome(Some(0)), InstallOutcome::Success);
        assert_eq!(installer_outcome(Some(3010)), InstallOutcome::NeedsReboot);
        assert_eq!(installer_outcome(Some(1)), InstallOutcome::Failed);
        assert_eq!(installer_outcome(None), InstallOutcome::Failed);

        let status = RemoteSoftwareStatus::success("request-1", InstallOutcome::NeedsReboot);
        assert_eq!(status.request_id, "request-1");
        assert_eq!(status.stage, RemoteSoftwareStage::NeedsReboot);
        assert!(status.needs_reboot);
        assert_eq!(status.progress_percent, Some(100));
    }
}
