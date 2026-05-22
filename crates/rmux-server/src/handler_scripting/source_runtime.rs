use std::path::Path;
use std::sync::atomic::Ordering;

use rmux_core::{
    command_parser::{CommandParser, ParsedCommands},
    formats::FormatContext,
};
use rmux_proto::{
    CommandOutput, ErrorResponse, PaneTarget, Response, RmuxError, SourceFileRequest,
    SourceFileResponse, Target,
};

use super::super::RequestHandler;
use super::format_context::{format_context_for_target, parser_with_parse_time_context};
use super::queue::{QueueCommandAction, QueueExecutionContext};
use super::source_files::{
    default_config_paths, default_tmux_fallback_paths, source_inputs_for_path, source_parse_error,
    LoadedSourceFile, ParsedSourceFileCommand, SourceInput, SourceSyntax, SourcedParsedCommands,
};
use super::targets::active_session_target;
use super::tmux_compat::filter_tmux_compat_input;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::{ConfigFileSelection, ConfigLoadOptions};

impl RequestHandler {
    pub(crate) async fn load_startup_config(&self, config_load: ConfigLoadOptions) {
        self.config_loading_depth.fetch_add(1, Ordering::Relaxed);
        let queue_errors = !matches!(config_load.selection(), ConfigFileSelection::Files(_));
        let (paths, tmux_fallback_paths) = match config_load.selection() {
            ConfigFileSelection::Disabled => {
                self.config_loading_depth.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            ConfigFileSelection::Default => (default_config_paths(), default_tmux_fallback_paths()),
            ConfigFileSelection::Files(files) => (
                files
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                Vec::new(),
            ),
        };

        let command = ParsedSourceFileCommand {
            paths,
            quiet: config_load.quiet(),
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: config_load.cwd().map(Path::to_path_buf),
            stdin: None,
            current_file: None,
            syntax: SourceSyntax::Rmux,
        };

        let loaded = match self.load_source_file_command(&command, 1).await {
            Ok(loaded) => loaded,
            Err(error) => {
                if queue_errors {
                    self.startup_config_errors.lock().await.push(error);
                }
                self.config_loading_depth.fetch_sub(1, Ordering::Relaxed);
                return;
            }
        };

        let should_load_tmux_fallback =
            !loaded.loaded_any_file() && !loaded.has_errors() && !tmux_fallback_paths.is_empty();
        let mut loaded = if should_load_tmux_fallback {
            let fallback_command = ParsedSourceFileCommand {
                paths: tmux_fallback_paths,
                quiet: true,
                syntax: SourceSyntax::TmuxCompat,
                ..command.clone()
            };
            match self.load_source_file_command(&fallback_command, 1).await {
                Ok(loaded) => loaded,
                Err(_) => {
                    self.config_loading_depth.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            }
        } else {
            loaded
        };

        let mut errors = Vec::new();
        if let Some(error) = loaded.take_error() {
            errors.push(error);
        }
        if let Err(error) = self
            .execute_loaded_source_file(
                std::process::id(),
                loaded,
                QueueExecutionContext::new(command.caller_cwd.clone()),
                1,
            )
            .await
        {
            errors.push(error);
        }
        if queue_errors
            && let Some(error) = super::aggregate_rmux_errors(errors) {
                self.startup_config_errors.lock().await.push(error);
            }
        self.config_loading_depth.fetch_sub(1, Ordering::Relaxed);
    }

    pub(in crate::handler) async fn handle_source_file(
        &self,
        requester_pid: u32,
        request: SourceFileRequest,
    ) -> Response {
        let mut command = ParsedSourceFileCommand::from(request);
        if command.target.is_none() {
            command.target = self.implicit_source_file_target(requester_pid).await;
        }
        let mut loaded = match self.load_source_file_command(&command, 1).await {
            Ok(loaded) => loaded,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let mut errors = Vec::new();
        if let Some(error) = loaded.take_error() {
            errors.push(error);
        }

        let mut stdout = std::mem::take(&mut loaded.stdout);
        if !command.parse_only {
            match self
                .execute_loaded_source_file(
                    requester_pid,
                    loaded,
                    QueueExecutionContext::new(command.caller_cwd.clone())
                        .with_current_target(command.target.clone().map(Target::Pane)),
                    1,
                )
                .await
            {
                Ok(output) => stdout.extend_from_slice(output.stdout()),
                Err(error) => errors.push(error),
            }
        }

        if let Some(error) = super::aggregate_rmux_errors(errors) {
            return Response::Error(ErrorResponse { error });
        }

        if stdout.is_empty() {
            Response::SourceFile(SourceFileResponse::no_output())
        } else {
            Response::SourceFile(SourceFileResponse::from_output(CommandOutput::from_stdout(
                stdout,
            )))
        }
    }

    async fn implicit_source_file_target(&self, requester_pid: u32) -> Option<PaneTarget> {
        let session_name = match self.current_session_candidate(requester_pid).await {
            Some(session_name) => Some(session_name),
            None => self.preferred_session_name().await.ok(),
        }?;
        let state = self.state.lock().await;
        match active_session_target(&state.sessions, &session_name) {
            Some(Target::Pane(target)) => Some(target),
            _ => None,
        }
    }

    pub(super) async fn execute_queued_source_file(
        &self,
        _requester_pid: u32,
        mut command: ParsedSourceFileCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let depth = context.source_file_depth.saturating_add(1);
        command.current_file = context.current_file.clone();
        let mut loaded = self.load_source_file_command(&command, depth).await?;
        let error = loaded.take_error();

        if command.parse_only || loaded.is_empty() {
            return Ok(QueueCommandAction::Normal {
                output: nonempty_stdout(loaded.stdout),
                error,
            });
        }

        Ok(QueueCommandAction::InsertAfter {
            batches: loaded
                .commands
                .into_iter()
                .map(|batch| {
                    (
                        batch.commands,
                        context.for_sourced_commands(depth, batch.current_file),
                    )
                })
                .collect(),
            output: nonempty_stdout(loaded.stdout),
            error,
        })
    }

    async fn execute_loaded_source_file(
        &self,
        requester_pid: u32,
        loaded: LoadedSourceFile,
        context: QueueExecutionContext,
        depth: usize,
    ) -> Result<CommandOutput, RmuxError> {
        let mut stdout = Vec::new();
        let mut errors = Vec::new();
        for batch in loaded.commands {
            match self
                .execute_parsed_commands(
                    requester_pid,
                    batch.commands,
                    context.for_sourced_commands(depth, batch.current_file),
                )
                .await
            {
                Ok(output) => stdout.extend_from_slice(output.stdout()),
                Err(error) => errors.push(error),
            }
        }

        match super::aggregate_rmux_errors(errors) {
            Some(error) => Err(error),
            None => Ok(CommandOutput::from_stdout(stdout)),
        }
    }

    async fn load_source_file_command(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
    ) -> Result<LoadedSourceFile, RmuxError> {
        if depth > super::SOURCE_FILE_NESTING_LIMIT {
            return Err(RmuxError::Server("too many nested files".to_owned()));
        }

        let mut loaded = LoadedSourceFile::default();

        for path in &command.paths {
            let expanded_path = if command.expand_paths {
                self.render_source_file_path(
                    path,
                    command.target.as_ref(),
                    command.current_file.as_deref(),
                )
                .await?
            } else {
                path.clone()
            };
            let inputs = match source_inputs_for_path(
                &expanded_path,
                command.caller_cwd.as_deref(),
                command.quiet,
                command.stdin.as_deref(),
                command.read_policy(),
            ) {
                Ok(inputs) => inputs,
                Err(error) => {
                    loaded.push_error(error);
                    continue;
                }
            };
            if !inputs.is_empty() {
                loaded.record_loaded_files(inputs.len());
            }
            for input in inputs {
                let input = match command.syntax {
                    SourceSyntax::Rmux => input,
                    SourceSyntax::TmuxCompat => filter_tmux_compat_input(&input),
                };
                if input.contents.trim().is_empty() {
                    continue;
                }
                let parsed = match self
                    .parse_source_input(&input, command.target.as_ref())
                    .await
                {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        loaded.push_error(error);
                        continue;
                    }
                };
                if command.verbose {
                    append_verbose_commands(&mut loaded.stdout, &parsed);
                }
                if !command.parse_only {
                    loaded.commands.push(SourcedParsedCommands {
                        commands: parsed,
                        current_file: Some(input.current_file.clone()),
                    });
                }
            }
        }

        Ok(loaded)
    }

    async fn render_source_file_path(
        &self,
        path: &str,
        target: Option<&PaneTarget>,
        current_file: Option<&str>,
    ) -> Result<String, RmuxError> {
        let attached_count = if let Some(target) = target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };
        let state = self.state.lock().await;
        let mut context = match target {
            Some(target) => {
                format_context_for_target(&state, &Target::Pane(target.clone()), attached_count)?
            }
            None => RuntimeFormatContext::new(FormatContext::new()).with_state(&state),
        };

        if let Some(current_file) = current_file {
            context = context.with_named_value("current_file", current_file);
        }
        Ok(render_runtime_template(path, &context, false))
    }

    async fn parse_source_input(
        &self,
        input: &SourceInput,
        target: Option<&PaneTarget>,
    ) -> Result<ParsedCommands, RmuxError> {
        let attached_count = if let Some(target) = target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };
        let state = self.state.lock().await;
        let mut parser = CommandParser::new().with_environment_store(&state.environment);
        let context = match target {
            Some(target) => {
                format_context_for_target(&state, &Target::Pane(target.clone()), attached_count)?
                    .with_named_value("current_file", &input.current_file)
            }
            None => RuntimeFormatContext::new(
                FormatContext::new().with_named_value("current_file", &input.current_file),
            )
            .with_state(&state),
        };
        parser = parser_with_parse_time_context(parser, &context);
        parser
            .parse(&input.contents)
            .map_err(|error| source_parse_error(input, error))
    }
}

fn append_verbose_commands(stdout: &mut Vec<u8>, parsed: &ParsedCommands) {
    if parsed.is_empty() {
        return;
    }
    stdout.extend_from_slice(parsed.to_tmux_string().as_bytes());
    stdout.push(b'\n');
}

fn nonempty_stdout(stdout: Vec<u8>) -> Option<CommandOutput> {
    if stdout.is_empty() {
        None
    } else {
        Some(CommandOutput::from_stdout(stdout))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::super::super::RequestHandler;
    use crate::test_env::EnvVarGuard;
    use crate::DaemonConfig;
    use rmux_proto::OptionName;

    #[tokio::test]
    async fn tmux_fallback_is_not_used_after_rmux_config_load_error() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-load-error");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        fs::create_dir(rmux_user_config_path(&home))
            .expect("directory that read_to_string rejects");
        write_test_config(tmux_user_config_path(&home), "set -g status off\n");

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let errors = handler.startup_config_errors.lock().await;
        let rendered = errors
            .first()
            .expect("rmux config load error should be retained")
            .to_string();
        assert!(
            rendered.contains(".rmux.conf"),
            "expected rmux config load error, got {rendered}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tmux_fallback_imports_filtered_static_config_when_no_rmux_config_exists() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-static-config");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        write_test_config(
            tmux_user_config_path(&home),
            "unbind-key -a\n\
             if-shell 'test -f ~/.enable-rmux' {\n\
             set -g status on\n\
             }\n\
             set -g status off\n\
             run-shell 'touch /tmp/nope'\n",
        );

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "tmux fallback import should be best-effort and error-free"
        );
        let state = handler.state.lock().await;
        assert_eq!(state.options.global_value(OptionName::Status), Some("off"));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tmux_fallback_ignores_unreadable_entries_and_keeps_later_safe_files() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-best-effort");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        create_test_dir_entry(first_tmux_fallback_path(&home, &xdg, &appdata));
        write_test_config(
            later_tmux_fallback_path(&home, &xdg, &appdata),
            "set -g status off\n",
        );

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "tmux fallback read errors should be ignored"
        );
        let state = handler.state.lock().await;
        assert_eq!(state.options.global_value(OptionName::Status), Some("off"));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tmux_fallback_can_be_disabled_by_env() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-env-disabled");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        write_test_config(tmux_user_config_path(&home), "set -g status off\n");

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, Some("1"));
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "disabled tmux fallback should not report config errors"
        );
        let state = handler.state.lock().await;
        assert_ne!(state.options.global_value(OptionName::Status), Some("off"));

        let _ = fs::remove_dir_all(root);
    }

    fn unique_temp_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("rmux-{label}-{}-{unique}", std::process::id()))
    }

    struct TestConfigEnv {
        _disable: EnvVarGuard,
        _home: EnvVarGuard,
        _xdg: EnvVarGuard,
        _userprofile: EnvVarGuard,
        _appdata: EnvVarGuard,
        _rmux_config: EnvVarGuard,
    }

    impl TestConfigEnv {
        fn install(
            home: &Path,
            xdg: &Path,
            appdata: &Path,
            disable_tmux_fallback: Option<&str>,
        ) -> Self {
            let home = home.to_string_lossy();
            let xdg = xdg.to_string_lossy();
            let appdata = appdata.to_string_lossy();

            Self {
                _disable: EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", disable_tmux_fallback),
                _home: EnvVarGuard::set("HOME", Some(&home)),
                _xdg: EnvVarGuard::set("XDG_CONFIG_HOME", Some(&xdg)),
                _userprofile: EnvVarGuard::set("USERPROFILE", Some(&home)),
                _appdata: EnvVarGuard::set("APPDATA", Some(&appdata)),
                _rmux_config: EnvVarGuard::set("RMUX_CONFIG_FILE", None),
            }
        }
    }

    fn create_test_config_dirs(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let home = root.join("home");
        let xdg = root.join("xdg");
        let appdata = root.join("appdata");
        fs::create_dir_all(&home).expect("home directory");
        fs::create_dir_all(&xdg).expect("xdg directory");
        fs::create_dir_all(&appdata).expect("appdata directory");
        (home, xdg, appdata)
    }

    fn rmux_user_config_path(home: &Path) -> PathBuf {
        home.join(".rmux.conf")
    }

    fn tmux_user_config_path(home: &Path) -> PathBuf {
        home.join(".tmux.conf")
    }

    #[cfg(windows)]
    fn first_tmux_fallback_path(_home: &Path, xdg: &Path, _appdata: &Path) -> PathBuf {
        xdg.join("tmux").join("tmux.conf")
    }

    #[cfg(not(windows))]
    fn first_tmux_fallback_path(home: &Path, _xdg: &Path, _appdata: &Path) -> PathBuf {
        home.join(".tmux.conf")
    }

    #[cfg(windows)]
    fn later_tmux_fallback_path(home: &Path, _xdg: &Path, _appdata: &Path) -> PathBuf {
        home.join(".tmux.conf")
    }

    #[cfg(not(windows))]
    fn later_tmux_fallback_path(_home: &Path, xdg: &Path, _appdata: &Path) -> PathBuf {
        xdg.join("tmux").join("tmux.conf")
    }

    fn create_test_dir_entry(path: PathBuf) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("test config parent directory");
        }
        fs::create_dir(path).expect("unreadable directory entry");
    }

    fn write_test_config(path: PathBuf, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("test config parent directory");
        }
        fs::write(path, contents).expect("test config file");
    }
}
