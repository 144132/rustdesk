#[cfg(windows)]
use hbb_common::config::{keys, Config};

pub(crate) fn format_device_name(custom_name: &str, hostname: &str) -> String {
    let custom_name = custom_name.trim();
    let hostname = hostname.trim();

    if custom_name.is_empty() {
        hostname.to_owned()
    } else if hostname.is_empty() {
        custom_name.to_owned()
    } else {
        format!("{custom_name}（{hostname}）")
    }
}

pub(crate) fn remote_device_name() -> String {
    #[cfg(windows)]
    {
        let custom_name = Config::get_option(keys::OPTION_PRESET_DEVICE_NAME);
        return format_device_name(&custom_name, &crate::common::whoami_hostname());
    }

    #[cfg(any(target_os = "android", target_os = "ios"))]
    return crate::common::hostname();

    #[cfg(all(
        not(windows),
        not(target_os = "android"),
        not(target_os = "ios")
    ))]
    crate::common::whoami_hostname()
}

#[cfg(test)]
mod tests {
    use super::format_device_name;

    #[test]
    fn custom_name_is_combined_with_windows_hostname() {
        assert_eq!(
            format_device_name("一号教室电脑", "DESKTOP-ABC123"),
            "一号教室电脑（DESKTOP-ABC123）"
        );
    }

    #[test]
    fn empty_custom_name_uses_windows_hostname() {
        assert_eq!(format_device_name("", "DESKTOP-ABC123"), "DESKTOP-ABC123");
    }

    #[test]
    fn whitespace_custom_name_uses_windows_hostname() {
        assert_eq!(
            format_device_name("  ", "DESKTOP-ABC123"),
            "DESKTOP-ABC123"
        );
    }
}
