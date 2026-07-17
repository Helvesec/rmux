use serde_json::{json, Value};

use super::super::ExitFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaneExitStatus {
    stale: bool,
    exit_status: Option<i32>,
    exit_signal: Option<i32>,
}

impl PaneExitStatus {
    pub(super) const fn stale() -> Self {
        Self {
            stale: true,
            exit_status: None,
            exit_signal: None,
        }
    }

    pub(super) const fn known(exit_status: Option<i32>, exit_signal: Option<i32>) -> Self {
        Self {
            stale: false,
            exit_status,
            exit_signal,
        }
    }

    pub(super) fn json_value(self) -> Value {
        json!({
            "stale": self.stale,
            "exit_status": self.exit_status,
            "exit_signal": self.exit_signal,
        })
    }

    pub(super) fn send_keys_exit_code(observation: Option<Self>) -> Result<i32, ExitFailure> {
        let observation = observation.ok_or_else(|| {
            ExitFailure::new(
                1,
                "send-keys observed pane exit without process exit metadata",
            )
        })?;
        if let Some(exit_status) = observation.exit_status {
            return Ok(exit_status);
        }
        if let Some(exit_signal) = observation.exit_signal.filter(|signal| *signal > 0) {
            return 128_i32.checked_add(exit_signal).ok_or_else(|| {
                ExitFailure::new(
                    1,
                    format!("send-keys observed invalid pane exit signal {exit_signal}"),
                )
            });
        }

        let detail = if observation.stale {
            " because the pane target became stale"
        } else {
            ""
        };
        Err(ExitFailure::new(
            1,
            format!("send-keys could not determine the pane process exit status{detail}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::PaneExitStatus;

    #[test]
    fn normal_exit_codes_are_preserved_without_cli_truncation() {
        for exit_status in [0, 7, 513] {
            assert_eq!(
                PaneExitStatus::send_keys_exit_code(Some(PaneExitStatus::known(
                    Some(exit_status),
                    None,
                )))
                .expect("normal exit status must be available"),
                exit_status
            );
        }
    }

    #[test]
    fn unix_signal_uses_conventional_shell_exit_code() {
        assert_eq!(
            PaneExitStatus::send_keys_exit_code(Some(PaneExitStatus::known(None, Some(15))))
                .expect("valid signal must map to a shell exit code"),
            143
        );
    }

    #[test]
    fn missing_or_unknown_exit_metadata_fails_closed() {
        for observation in [
            None,
            Some(PaneExitStatus::stale()),
            Some(PaneExitStatus::known(None, None)),
            Some(PaneExitStatus::known(None, Some(0))),
        ] {
            let error = PaneExitStatus::send_keys_exit_code(observation)
                .expect_err("unknown exit outcome must not become success");
            assert_eq!(error.exit_code(), 1);
        }
    }

    #[test]
    fn serialized_exit_metadata_keeps_the_existing_json_shape() {
        let value = PaneExitStatus::known(Some(7), None).json_value();

        assert_eq!(
            value,
            json!({
                "stale": false,
                "exit_status": 7,
                "exit_signal": null,
            })
        );
    }
}
