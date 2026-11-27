//! Shell and sourced-command responses: `run-shell`, `if-shell` and `source-file`.

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use super::CommandOutput;

/// Response payload for `run-shell`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunShellResponse {
    /// Captured stdout for foreground `run-shell`.
    pub output: Option<CommandOutput>,
    /// Exit status returned by a foreground shell command.
    pub exit_status: Option<i32>,
}

impl RunShellResponse {
    /// Builds a background `run-shell` response.
    #[must_use]
    pub const fn background() -> Self {
        Self {
            output: None,
            exit_status: None,
        }
    }

    /// Builds a response with no command output.
    #[must_use]
    pub const fn no_output() -> Self {
        Self::background()
    }

    /// Builds a foreground `run-shell` response.
    #[must_use]
    pub fn from_output(output: CommandOutput) -> Self {
        Self {
            output: Some(output),
            exit_status: Some(0),
        }
    }

    /// Builds a foreground shell response with an exact exit status.
    #[must_use]
    pub const fn from_exit_status(exit_status: i32) -> Self {
        Self {
            output: None,
            exit_status: Some(exit_status),
        }
    }

    /// Builds a foreground shell response with output and an exact exit status.
    #[must_use]
    pub fn from_output_and_exit_status(output: CommandOutput, exit_status: i32) -> Self {
        Self {
            output: Some(output),
            exit_status: Some(exit_status),
        }
    }

    /// Returns the reusable stdout payload for foreground `run-shell`.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }

    /// Returns the shell exit status when the response came from a foreground shell.
    #[must_use]
    pub const fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }
}

impl<'de> Deserialize<'de> for RunShellResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "RunShellResponse",
            &["output", "exit_status"],
            RunShellResponseVisitor,
        )
    }
}

struct RunShellResponseVisitor;

impl<'de> Visitor<'de> for RunShellResponseVisitor {
    type Value = RunShellResponse;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a run-shell response")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let output = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let exit_status = compat_next_response_element(&mut seq)?;
        Ok(RunShellResponse {
            output,
            exit_status,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut output = None;
        let mut exit_status = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "output" => output = Some(map.next_value()?),
                "exit_status" => exit_status = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(RunShellResponse {
            output: output.ok_or_else(|| de::Error::missing_field("output"))?,
            exit_status: exit_status.unwrap_or_default(),
        })
    }
}

fn compat_next_response_element<'de, A, T>(seq: &mut A) -> Result<T, A::Error>
where
    A: SeqAccess<'de>,
    T: Deserialize<'de> + Default,
{
    match seq.next_element::<T>() {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Ok(T::default()),
        Err(error) => Err(error),
    }
}

/// Response payload for `if-shell`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IfShellResponse {
    /// Captured stdout from the selected nested command when it produced output.
    pub output: Option<CommandOutput>,
}

impl IfShellResponse {
    /// Builds a response with no nested command output.
    #[must_use]
    pub const fn no_output() -> Self {
        Self { output: None }
    }

    /// Builds a response with nested command output.
    #[must_use]
    pub fn from_output(output: CommandOutput) -> Self {
        Self {
            output: Some(output),
        }
    }

    /// Returns the reusable stdout payload when the selected nested command printed output.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }
}

/// Response payload for `source-file`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SourceFileResponse {
    /// Verbose parsed-command output, when requested.
    pub output: Option<CommandOutput>,
    /// Exit status surfaced by a nested foreground command.
    pub exit_status: Option<i32>,
    /// File-loading diagnostics that tmux writes to stderr.
    pub stderr: Vec<u8>,
}

impl SourceFileResponse {
    /// Builds a response without command output.
    #[must_use]
    pub const fn no_output() -> Self {
        Self {
            output: None,
            stderr: Vec::new(),
            exit_status: None,
        }
    }

    /// Builds a response with command output.
    #[must_use]
    pub fn from_output(output: CommandOutput) -> Self {
        Self {
            output: Some(output),
            stderr: Vec::new(),
            exit_status: None,
        }
    }

    /// Returns the reusable stdout payload when `source-file -v` printed output.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }

    /// Returns bytes to write to stderr for file-loading diagnostics.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Adds stderr bytes to the response.
    #[must_use]
    pub fn with_stderr(mut self, stderr: Vec<u8>) -> Self {
        self.stderr = stderr;
        self
    }

    /// Adds a command exit status to the response.
    #[must_use]
    pub const fn with_exit_status(mut self, exit_status: Option<i32>) -> Self {
        self.exit_status = exit_status;
        self
    }

    /// Returns the surfaced nested command exit status, if any.
    #[must_use]
    pub const fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }
}

impl<'de> Deserialize<'de> for SourceFileResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "SourceFileResponse",
            &["output", "exit_status", "stderr"],
            SourceFileResponseVisitor,
        )
    }
}

struct SourceFileResponseVisitor;

impl<'de> Visitor<'de> for SourceFileResponseVisitor {
    type Value = SourceFileResponse;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a source-file response")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let output = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let exit_status = compat_next_response_element(&mut seq)?;
        let stderr = compat_next_response_element(&mut seq)?;
        Ok(SourceFileResponse {
            output,
            exit_status,
            stderr,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut output = None;
        let mut exit_status = None;
        let mut stderr = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "output" => output = Some(map.next_value()?),
                "exit_status" => exit_status = Some(map.next_value()?),
                "stderr" => stderr = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(SourceFileResponse {
            output: output.ok_or_else(|| de::Error::missing_field("output"))?,
            exit_status: exit_status.unwrap_or_default(),
            stderr: stderr.unwrap_or_default(),
        })
    }
}
