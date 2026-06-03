use std::collections::HashMap;

#[cfg(not(windows))]
const CLIENT_SPAWN_ENVIRONMENT_NAMES: &[&str] = &["PATH", "SHELL"];
#[cfg(windows)]
const CLIENT_SPAWN_ENVIRONMENT_NAMES: &[&str] = &["PATH", "PATHEXT"];

pub(in crate::handler) fn client_spawn_environment(
    client_environment: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let client_environment = client_environment?;
    let mut spawn_environment = HashMap::new();

    for name in CLIENT_SPAWN_ENVIRONMENT_NAMES {
        if let Some((client_name, value)) = client_environment_entry(client_environment, name) {
            spawn_environment.insert(client_name.to_owned(), value.to_owned());
        }
    }

    (!spawn_environment.is_empty()).then_some(spawn_environment)
}

#[cfg(not(windows))]
fn client_environment_entry<'a>(
    client_environment: &'a HashMap<String, String>,
    name: &str,
) -> Option<(&'a str, &'a str)> {
    client_environment
        .get_key_value(name)
        .map(|(client_name, value)| (client_name.as_str(), value.as_str()))
}

#[cfg(windows)]
fn client_environment_entry<'a>(
    client_environment: &'a HashMap<String, String>,
    name: &str,
) -> Option<(&'a str, &'a str)> {
    client_environment
        .iter()
        .find(|(client_name, _)| client_name.eq_ignore_ascii_case(name))
        .map(|(client_name, value)| (client_name.as_str(), value.as_str()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::client_spawn_environment;

    #[test]
    fn client_spawn_environment_keeps_path_without_leaking_arbitrary_variables() {
        let client_environment = HashMap::from([
            ("PATH".to_owned(), "/tmp/client-bin:/usr/bin".to_owned()),
            (
                "RMUX_CLIENT_ENV_SENTINEL".to_owned(),
                "from-client".to_owned(),
            ),
        ]);

        let spawn_environment =
            client_spawn_environment(Some(&client_environment)).expect("PATH is retained");

        assert_eq!(
            spawn_environment.get("PATH").map(String::as_str),
            Some("/tmp/client-bin:/usr/bin")
        );
        assert!(!spawn_environment.contains_key("RMUX_CLIENT_ENV_SENTINEL"));
    }

    #[cfg(unix)]
    #[test]
    fn client_spawn_environment_keeps_shell_for_default_shell_resolution() {
        let client_environment = HashMap::from([
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("SHELL".to_owned(), "/usr/bin/fish".to_owned()),
        ]);

        let spawn_environment =
            client_spawn_environment(Some(&client_environment)).expect("spawn env is retained");

        assert_eq!(
            spawn_environment.get("SHELL").map(String::as_str),
            Some("/usr/bin/fish")
        );
    }

    #[test]
    fn client_spawn_environment_is_empty_without_spawn_relevant_names() {
        let client_environment = HashMap::from([(
            "RMUX_CLIENT_ENV_SENTINEL".to_owned(),
            "from-client".to_owned(),
        )]);

        assert_eq!(client_spawn_environment(Some(&client_environment)), None);
        assert_eq!(client_spawn_environment(None), None);
    }
}
