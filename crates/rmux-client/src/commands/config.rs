use rmux_proto::request::{SetHookMutationRequest, ShowHooksRequest};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    HookLifecycle, HookName, OptionName, PaneTarget, Request, Response, ScopeSelector,
    SetEnvironmentMode, SetEnvironmentRequest, SetHookRequest, SetOptionByNameRequest,
    SetOptionMode, SetOptionRequest, ShowEnvironmentRequest, ShowOptionsRequest, SourceFileRequest,
};
use std::path::PathBuf;

use crate::{connection::Connection, ClientError};

impl Connection {
    /// Sends a `set-option` request over the detached RPC channel.
    pub fn set_option(
        &mut self,
        scope: ScopeSelector,
        option: OptionName,
        value: String,
        mode: SetOptionMode,
    ) -> Result<Response, ClientError> {
        let request = SetOptionRequest {
            scope,
            option,
            value,
            mode,
        };
        rmux_core::validate_option_mutation(
            request.option,
            &request.scope,
            request.mode,
            &request.value,
        )?;
        self.roundtrip(&Request::SetOption(request))
    }

    /// Sends a string-keyed `set-option` request over the detached RPC channel.
    #[allow(clippy::too_many_arguments)]
    pub fn set_option_by_name(
        &mut self,
        scope: OptionScopeSelector,
        name: String,
        value: Option<String>,
        mode: SetOptionMode,
        only_if_unset: bool,
        unset: bool,
        unset_pane_overrides: bool,
    ) -> Result<Response, ClientError> {
        let request = SetOptionByNameRequest {
            scope,
            name,
            value,
            mode,
            only_if_unset,
            unset,
            unset_pane_overrides,
            format: false,
            format_target: None,
        };
        rmux_core::validate_option_name_mutation(
            &request.name,
            &request.scope,
            request.mode,
            request.value.as_deref(),
            request.unset,
        )?;
        self.roundtrip(&Request::SetOptionByName(request))
    }

    /// Sends a `set-environment` request over the detached RPC channel.
    pub fn set_environment(
        &mut self,
        scope: ScopeSelector,
        name: String,
        value: String,
        mode: Option<SetEnvironmentMode>,
        hidden: bool,
        format: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SetEnvironment(SetEnvironmentRequest {
            scope,
            name,
            value,
            mode,
            hidden,
            format,
        }))
    }

    /// Sends a `set-hook` request over the detached RPC channel.
    pub fn set_hook(
        &mut self,
        scope: ScopeSelector,
        hook: HookName,
        command: String,
        lifecycle: HookLifecycle,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SetHook(SetHookRequest {
            scope,
            hook,
            command,
            lifecycle,
        }))
    }

    /// Sends an extended `set-hook` mutation over the detached RPC channel.
    #[allow(clippy::too_many_arguments)]
    pub fn set_hook_mutation(
        &mut self,
        scope: ScopeSelector,
        hook: HookName,
        command: Option<String>,
        lifecycle: HookLifecycle,
        append: bool,
        unset: bool,
        run_immediately: bool,
        index: Option<u32>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SetHookMutation(SetHookMutationRequest {
            scope,
            hook,
            command,
            lifecycle,
            append,
            unset,
            run_immediately,
            index,
        }))
    }

    /// Sends a `show-options` request over the detached RPC channel.
    pub fn show_options(
        &mut self,
        scope: OptionScopeSelector,
        name: Option<String>,
        value_only: bool,
        include_inherited: bool,
        quiet: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ShowOptions(ShowOptionsRequest {
            scope,
            name,
            value_only,
            include_inherited,
            quiet,
        }))
    }

    /// Sends a `show-environment` request over the detached RPC channel.
    pub fn show_environment(
        &mut self,
        scope: ScopeSelector,
        name: Option<String>,
        hidden: bool,
        shell_format: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ShowEnvironment(ShowEnvironmentRequest {
            scope,
            name,
            hidden,
            shell_format,
        }))
    }

    /// Sends a `show-hooks` request over the detached RPC channel.
    pub fn show_hooks(
        &mut self,
        scope: ScopeSelector,
        window: bool,
        pane: bool,
        hook: Option<HookName>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ShowHooks(ShowHooksRequest {
            scope,
            window,
            pane,
            hook,
        }))
    }

    /// Sends a `source-file` request over the detached RPC channel.
    #[allow(clippy::too_many_arguments)]
    pub fn source_file(
        &mut self,
        paths: Vec<String>,
        quiet: bool,
        parse_only: bool,
        verbose: bool,
        expand_paths: bool,
        target: Option<PaneTarget>,
        stdin: Option<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::SourceFile(SourceFileRequest {
            paths,
            quiet,
            parse_only,
            verbose,
            expand_paths,
            target,
            caller_cwd: current_working_directory(),
            stdin,
        }))
    }
}

fn current_working_directory() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{self, Read};
    use std::os::unix::net::UnixStream;

    use rmux_proto::{OptionName, RmuxError, ScopeSelector, SessionName, SetOptionMode};

    use super::Connection;
    use crate::ClientError;

    #[test]
    fn set_option_rejects_invalid_requests_before_writing_to_the_socket() {
        let (client_stream, mut server_stream) = UnixStream::pair().expect("create stream pair");
        server_stream
            .set_nonblocking(true)
            .expect("set read end nonblocking");
        let mut connection = Connection::new(client_stream).expect("connection");

        let error = connection
            .set_option(
                ScopeSelector::Session(SessionName::new("alpha").expect("valid session")),
                OptionName::DefaultTerminal,
                "tmux-256color".to_owned(),
                SetOptionMode::Replace,
            )
            .expect_err("invalid request should fail");

        assert!(matches!(
            error,
            ClientError::Protocol(RmuxError::InvalidSetOption(message))
                if message == "default-terminal is only supported at global scope"
        ));

        let mut buffer = [0_u8; 1];
        let read_error = server_stream
            .read(&mut buffer)
            .expect_err("validation should happen before any bytes are written");
        assert_eq!(read_error.kind(), io::ErrorKind::WouldBlock);
    }
}
