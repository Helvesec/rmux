use std::future::{Future, IntoFuture};
use std::path::PathBuf;
use std::pin::Pin;

use crate::handles::session::{unexpected_response, Session};
use crate::transport::TransportClient;
use crate::{ProcessCommandSpec, Result, RmuxError, SessionName, Window, WindowRef};
use rmux_proto::{NewWindowRequest, ProcessCommand, Request, Response};

/// Builder returned by [`Session::new_window_with`].
#[derive(Debug)]
pub struct NewWindowBuilder<'a> {
    session: &'a Session,
    name: Option<String>,
    detached: bool,
    target_window_index: Option<u32>,
    insert_at_target: bool,
    process_command: Option<ProcessCommandSpec>,
    start_directory: Option<PathBuf>,
    environment: Option<Vec<String>>,
}

impl<'a> NewWindowBuilder<'a> {
    pub(crate) fn new(session: &'a Session) -> Self {
        Self {
            session,
            name: None,
            detached: false,
            target_window_index: None,
            insert_at_target: false,
            process_command: None,
            start_directory: None,
            environment: None,
        }
    }

    /// Sets the new window name atomically with the creation request.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Spawns the new window with the supplied argv.
    ///
    /// This is direct argv execution, including one-element argv. Use
    /// [`Self::shell`] when shell execution is intentional.
    #[must_use]
    pub fn spawn<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.process_command = Some(ProcessCommandSpec::Argv(
            command.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Spawns the new window through the user's shell.
    #[must_use]
    pub fn shell(mut self, command: impl Into<String>) -> Self {
        self.process_command = Some(ProcessCommandSpec::Shell(command.into()));
        self
    }

    /// Sets the process working directory for the new window's initial pane.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.start_directory = Some(cwd.into());
        self
    }

    /// Adds one process environment override for the new window's initial pane.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.get_or_insert_with(Vec::new).push(format!(
            "{}={}",
            key.into(),
            value.into()
        ));
        self
    }

    /// Controls whether the new window remains inactive after creation.
    ///
    /// The default is `false`, matching tmux `new-window`: the new window
    /// becomes active.
    #[must_use]
    pub const fn detached(mut self, detached: bool) -> Self {
        self.detached = detached;
        self
    }

    /// Requests a specific destination window index.
    #[must_use]
    pub const fn at_index(mut self, window_index: u32) -> Self {
        self.target_window_index = Some(window_index);
        self
    }

    /// Controls whether an occupied requested index shifts existing windows.
    ///
    /// This mirrors tmux insertion behavior and is meaningful only together
    /// with [`Self::at_index`].
    #[must_use]
    pub const fn insert(mut self, insert: bool) -> Self {
        self.insert_at_target = insert;
        self
    }

    async fn run(self) -> Result<Window> {
        let transport = self.session.transport().begin_operation();
        let process_command = proto_process_command(self.process_command)?;
        crate::capabilities::require_process_command_if_present(
            &transport,
            process_command.as_ref(),
        )
        .await?;
        let target = create_window(
            &transport,
            self.session.name().clone(),
            NewWindowConfig {
                name: self.name,
                detached: self.detached,
                environment: self.environment,
                process_command,
                start_directory: self.start_directory,
                target_window_index: self.target_window_index,
                insert_at_target: self.insert_at_target,
            },
        )
        .await?;

        Ok(Window::new(
            target,
            self.session.endpoint().clone(),
            self.session.configured_default_timeout(),
            transport,
        ))
    }
}

impl<'a> IntoFuture for NewWindowBuilder<'a> {
    type Output = Result<Window>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

struct NewWindowConfig {
    name: Option<String>,
    detached: bool,
    environment: Option<Vec<String>>,
    process_command: Option<ProcessCommand>,
    start_directory: Option<PathBuf>,
    target_window_index: Option<u32>,
    insert_at_target: bool,
}

async fn create_window(
    client: &TransportClient,
    target: SessionName,
    config: NewWindowConfig,
) -> Result<WindowRef> {
    match client
        .request(Request::NewWindow(Box::new(NewWindowRequest {
            target,
            name: config.name,
            detached: config.detached,
            environment: config.environment,
            command: None,
            process_command: config.process_command,
            start_directory: config.start_directory,
            target_window_index: config.target_window_index,
            insert_at_target: config.insert_at_target,
        })))
        .await?
    {
        Response::NewWindow(response) => Ok(response.target.into()),
        response => Err(unexpected_response("new-window", response)),
    }
}

fn proto_process_command(
    process_command: Option<ProcessCommandSpec>,
) -> Result<Option<ProcessCommand>> {
    let Some(process_command) = process_command else {
        return Ok(None);
    };
    if process_command.is_empty() {
        return Err(RmuxError::SpawnFailed {
            message: rmux_proto::PROCESS_COMMAND_EMPTY_MESSAGE.to_owned(),
        });
    }

    Ok(Some(process_command.into()))
}
