use std::ffi::OsString;

pub(super) fn client_environment_assignments() -> Vec<String> {
    let mut assignments = environment_assignments_from_pairs(std::env::vars_os());
    assignments.sort_unstable();
    assignments
}

fn environment_assignments_from_pairs<I>(pairs: I) -> Vec<String>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    pairs
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            if name.is_empty() || name.starts_with('=') {
                return None;
            }
            Some(format!("{}={}", name, value.into_string().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::client_environment_assignments;
    use super::environment_assignments_from_pairs;
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn client_environment_assignments_are_name_value_pairs() {
        let assignments = client_environment_assignments();

        assert!(assignments.iter().all(|value| value.contains('=')));
    }

    #[test]
    fn client_environment_assignments_are_stably_ordered() {
        let assignments = client_environment_assignments();
        let mut sorted = assignments.clone();
        sorted.sort_unstable();

        assert_eq!(assignments, sorted);
    }

    #[test]
    fn environment_assignments_skip_empty_and_windows_pseudo_names() {
        let assignments = environment_assignments_from_pairs([
            (OsString::from(""), OsString::from("empty")),
            (OsString::from("=C:"), OsString::from(r"C:\workspace")),
            (OsString::from("VALID"), OsString::from("value")),
        ]);

        assert_eq!(assignments, ["VALID=value"]);
    }

    #[cfg(unix)]
    #[test]
    fn environment_assignments_skip_non_utf8_pairs() {
        let assignments = environment_assignments_from_pairs([
            (
                OsString::from_vec(b"INVALID_NAME_\xff".to_vec()),
                OsString::from("value"),
            ),
            (
                OsString::from("INVALID_VALUE"),
                OsString::from_vec(b"value_\xff".to_vec()),
            ),
            (OsString::from("VALID"), OsString::from("value")),
        ]);

        assert_eq!(assignments, ["VALID=value"]);
    }
}
