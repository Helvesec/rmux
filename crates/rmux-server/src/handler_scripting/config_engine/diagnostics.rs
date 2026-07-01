use rmux_proto::RmuxError;

pub(crate) fn config_error_lines(error: &RmuxError) -> Vec<String> {
    match error {
        RmuxError::Server(message) => message
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        other => vec![other.to_string()],
    }
}
