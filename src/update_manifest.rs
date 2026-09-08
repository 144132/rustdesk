use url::Url;

pub(crate) const UPDATE_MANIFEST_URL: &str =
    "https://update.szxinyu.com/rustdesk/latest.yml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LatestYmlManifest {
    pub(crate) version: String,
    package_paths: Vec<String>,
}

impl LatestYmlManifest {
    fn package_paths(&self) -> impl Iterator<Item = &str> {
        self.package_paths.iter().map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdatePackage {
    pub(crate) version: String,
    pub(crate) url: String,
}

pub(crate) fn parse_latest_yml(content: &str) -> Result<LatestYmlManifest, String> {
    let mut version = String::new();
    let mut package_paths = Vec::new();
    let mut current_file_url: Option<String> = None;
    let mut in_files = false;

    for (line_index, raw_line) in content.lines().enumerate() {
        let line_number = line_index + 1;
        let indentation = raw_line
            .chars()
            .take_while(|character| *character == ' ' || *character == '\t')
            .count();
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if indentation == 0 && trimmed == "files:" {
            in_files = true;
            continue;
        }

        if let Some(item) = trimmed.strip_prefix("- ") {
            if !in_files {
                return Err(format!("unexpected list item on line {line_number}"));
            }
            if let Some(url) = current_file_url.take() {
                package_paths.push(url);
            }
            let (key, value) = split_mapping(item)
                .ok_or_else(|| format!("invalid list item on line {line_number}"))?;
            if key == "url" {
                current_file_url = Some(unquote_scalar(value));
            }
            continue;
        }

        let Some((key, raw_value)) = split_mapping(trimmed) else {
            return Err(format!("invalid mapping on line {line_number}"));
        };
        let value = unquote_scalar(raw_value);

        if indentation == 0 {
            match key {
                "version" => version = value,
                "path" if !value.is_empty() => package_paths.push(value),
                _ => {}
            }
        } else if in_files && key == "url" {
            if let Some(url) = current_file_url.take() {
                package_paths.push(url);
            }
            current_file_url = Some(value);
        }
    }

    if let Some(url) = current_file_url {
        package_paths.push(url);
    }

    version = version
        .trim()
        .trim_start_matches(|character: char| character == 'v' || character == 'V')
        .to_owned();
    package_paths.retain(|path| !path.trim().is_empty());
    package_paths.dedup();

    if version.is_empty() {
        return Err("latest.yml is missing version".to_owned());
    }
    if package_paths.is_empty() {
        return Err("latest.yml is missing an installer path".to_owned());
    }

    Ok(LatestYmlManifest {
        version,
        package_paths,
    })
}

pub(crate) fn select_update_package(
    manifest_url: &str,
    manifest: &LatestYmlManifest,
    extension: &str,
    arch: &str,
) -> Result<UpdatePackage, String> {
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    let candidates: Vec<&str> = manifest
        .package_paths()
        .filter(|path| path_matches_extension(path, &extension))
        .collect();

    if candidates.is_empty() {
        return Err(format!(
            "latest.yml has no {} installer for this platform",
            extension
        ));
    }

    let selected = candidates
        .iter()
        .copied()
        .find(|path| path_matches_arch(path, arch))
        .or_else(|| candidates.iter().copied().find(|path| path_is_universal(path)))
        .or_else(|| (candidates.len() == 1).then_some(candidates[0]))
        .ok_or_else(|| format!("latest.yml has no installer for architecture {arch}"))?;

    let url = resolve_download_url(manifest_url, selected)?;
    Ok(UpdatePackage {
        version: manifest.version.clone(),
        url,
    })
}

pub(crate) fn resolve_download_url(
    manifest_url: &str,
    relative_path: &str,
) -> Result<String, String> {
    let manifest = Url::parse(manifest_url).map_err(|error| error.to_string())?;
    let relative_path = relative_path.trim();
    if relative_path.is_empty()
        || relative_path.starts_with('/')
        || relative_path.contains('\\')
        || relative_path.contains('?')
        || relative_path.contains('#')
        || relative_path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err("installer path must be a safe relative path".to_owned());
    }

    let download = manifest
        .join(relative_path)
        .map_err(|error| error.to_string())?;
    if !same_origin(&manifest, &download)
        || download.query().is_some()
        || download.fragment().is_some()
    {
        return Err("installer URL must stay on the update server".to_owned());
    }

    let base_path = directory_prefix(&manifest);
    if !download.path().starts_with(&base_path) {
        return Err("installer URL escaped the manifest directory".to_owned());
    }

    Ok(download.to_string())
}

pub(crate) fn is_configured_update_download_url(url: &str) -> bool {
    let Ok(manifest) = Url::parse(UPDATE_MANIFEST_URL) else {
        return false;
    };
    let Ok(download) = Url::parse(url) else {
        return false;
    };
    if !same_origin(&manifest, &download)
        || !download.username().is_empty()
        || download.password().is_some()
        || download.query().is_some()
        || download.fragment().is_some()
    {
        return false;
    }

    let Some(path_segments) = download.path_segments() else {
        return false;
    };
    let segments: Vec<&str> = path_segments.collect();
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return false;
    }

    download.path().starts_with(&directory_prefix(&manifest))
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && normalized_port(left) == normalized_port(right)
}

fn normalized_port(url: &Url) -> Option<u16> {
    url.port().or_else(|| match url.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    })
}

fn directory_prefix(url: &Url) -> String {
    let base = url.path().rsplit_once('/').map_or("", |(base, _)| base);
    if base.is_empty() {
        "/".to_owned()
    } else {
        format!("{base}/")
    }
}

fn path_matches_extension(path: &str, extension: &str) -> bool {
    path.rsplit('/').next().is_some_and(|file_name| {
        file_name
            .rsplit_once('.')
            .is_some_and(|(_, file_extension)| file_extension.eq_ignore_ascii_case(extension))
    })
}

fn path_matches_arch(path: &str, arch: &str) -> bool {
    let path = path.to_ascii_lowercase();
    match arch {
        "x86_64" => path.contains("x86_64") || path.contains("x64") || path.contains("amd64"),
        "aarch64" => {
            path.contains("aarch64") || path.contains("arm64") || path.contains("arm-v8")
        }
        "arm" | "armv7" => {
            path.contains("armv7") || path.contains("armeabi") || path.contains("arm32")
        }
        "x86" | "i686" => {
            (path.contains("x86") && !path.contains("x86_64")) || path.contains("i686")
        }
        _ => false,
    }
}

fn path_is_universal(path: &str) -> bool {
    path.to_ascii_lowercase().contains("universal")
}

fn split_mapping(value: &str) -> Option<(&str, &str)> {
    let (key, value) = value.split_once(':')?;
    Some((key.trim(), strip_unquoted_comment(value).trim()))
}

fn unquote_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let matching_quotes = (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[value.len() - 1] == b'"');
        if matching_quotes {
            return value[1..value.len() - 1].to_owned();
        }
    }
    value.to_owned()
}

fn strip_unquoted_comment(value: &str) -> &str {
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        match character {
            '\\' if double_quoted => escaped = !escaped,
            '\'' if !double_quoted && !escaped => single_quoted = !single_quoted,
            '"' if !single_quoted && !escaped => double_quoted = !double_quoted,
            '#' if !single_quoted && !double_quoted && !escaped => return &value[..index],
            _ => escaped = false,
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        is_configured_update_download_url, parse_latest_yml, resolve_download_url,
        select_update_package,
    };

    const MANIFEST: &str = r#"version: 1.5.0
files:
  - url: xy-rustdesk-1.5.0-x86_64.exe
    sha512: abc
    size: 123
  - url: xy-rustdesk-1.5.0-aarch64.apk
    sha512: abc
    size: 123
  - url: xy-rustdesk-1.5.0-armv7.apk
    sha512: abc
    size: 123
  - url: xy-rustdesk-1.5.0-universal.apk
    sha512: abc
    size: 123
path: xy-rustdesk-1.5.0-x86_64.exe
sha512: abc
releaseDate: '2026-09-07T10:23:58.049Z'
"#;

    #[test]
    fn parses_latest_yml_version_and_installer_path() {
        let manifest = parse_latest_yml(MANIFEST).expect("valid latest.yml");
        assert_eq!(manifest.version, "1.5.0");

        let package = select_update_package(
            "https://update.szxinyu.com/rustdesk/latest.yml",
            &manifest,
            "exe",
            "x86_64",
        )
        .expect("matching Windows installer");
        assert_eq!(package.version, "1.5.0");
        assert_eq!(
            package.url,
            "https://update.szxinyu.com/rustdesk/xy-rustdesk-1.5.0-x86_64.exe"
        );
    }

    #[test]
    fn selects_android_package_for_target_architecture() {
        let manifest = parse_latest_yml(MANIFEST).expect("valid latest.yml");
        let package = select_update_package(
            "https://update.szxinyu.com/rustdesk/latest.yml",
            &manifest,
            "apk",
            "arm",
        )
        .expect("matching Android ARM installer");

        assert_eq!(
            package.url,
            "https://update.szxinyu.com/rustdesk/xy-rustdesk-1.5.0-armv7.apk"
        );
    }

    #[test]
    fn falls_back_to_universal_android_package() {
        let manifest = parse_latest_yml(
            "version: 1.5.0\nfiles:\n  - url: xy-rustdesk-1.5.0-universal.apk\n",
        )
        .expect("valid universal latest.yml");
        let package = select_update_package(
            "https://update.szxinyu.com/rustdesk/latest.yml",
            &manifest,
            "apk",
            "unknown",
        )
        .expect("universal Android installer");

        assert_eq!(
            package.url,
            "https://update.szxinyu.com/rustdesk/xy-rustdesk-1.5.0-universal.apk"
        );
    }

    #[test]
    fn rejects_manifest_paths_that_escape_update_directory() {
        assert!(resolve_download_url(
            "https://update.szxinyu.com/rustdesk/latest.yml",
            "../rustdesk.exe"
        )
        .is_err());
        assert!(resolve_download_url(
            "https://update.szxinyu.com/rustdesk/latest.yml",
            "https://evil.example/rustdesk.exe"
        )
        .is_err());
    }

    #[test]
    fn configured_update_download_url_requires_same_https_origin_and_path() {
        assert!(is_configured_update_download_url(
            "https://update.szxinyu.com/rustdesk/xy-rustdesk-1.5.0-x86_64.exe"
        ));
        assert!(!is_configured_update_download_url(
            "http://update.szxinyu.com/rustdesk/xy-rustdesk-1.5.0-x86_64.exe"
        ));
        assert!(!is_configured_update_download_url(
            "https://update.szxinyu.com/other/xy-rustdesk-1.5.0-x86_64.exe"
        ));
        assert!(!is_configured_update_download_url(
            "https://update.szxinyu.com/rustdesk//xy-rustdesk-1.5.0-x86_64.exe"
        ));
    }
}
