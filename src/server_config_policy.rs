use std::collections::HashMap;

use hbb_common::config::keys;

const LOCKED_OPTIONS: &[&str] = &[
    keys::OPTION_CUSTOM_RENDEZVOUS_SERVER,
    keys::OPTION_RELAY_SERVER,
    keys::OPTION_API_SERVER,
    keys::OPTION_KEY,
];

pub(crate) fn is_locked_option(key: &str) -> bool {
    LOCKED_OPTIONS.contains(&key)
}

pub(crate) fn is_locked_config_query(key: &str) -> bool {
    matches!(key, "rendezvous_server" | "rendezvous_servers")
}

pub(crate) fn remove_locked_options(options: &mut HashMap<String, String>) {
    for key in LOCKED_OPTIONS {
        options.remove(*key);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{is_locked_config_query, is_locked_option, remove_locked_options};
    use hbb_common::config::keys;

    #[test]
    fn locked_server_options_are_removed_from_client_options() {
        let locked = [
            keys::OPTION_CUSTOM_RENDEZVOUS_SERVER,
            keys::OPTION_RELAY_SERVER,
            keys::OPTION_API_SERVER,
            keys::OPTION_KEY,
        ];
        let mut options = locked
            .iter()
            .map(|key| ((*key).to_owned(), "user-value".to_owned()))
            .collect::<HashMap<_, _>>();
        options.insert("allow-websocket".to_owned(), "Y".to_owned());

        remove_locked_options(&mut options);

        assert!(locked.iter().all(|key| is_locked_option(key)));
        assert!(locked.iter().all(|key| !options.contains_key(*key)));
        assert_eq!(
            options.get("allow-websocket").map(String::as_str),
            Some("Y")
        );
        assert!(is_locked_config_query("rendezvous_server"));
        assert!(is_locked_config_query("rendezvous_servers"));
        assert!(!is_locked_config_query("id"));
    }
}
