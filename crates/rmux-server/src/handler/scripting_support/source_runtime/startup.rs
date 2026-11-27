use std::path::Path;

use rmux_proto::RmuxError;

use super::super::queue::QueueExecutionContext;
use super::super::source_files::{
    startup_config_paths, LoadedSourceFile, ParsedSourceFileCommand, SourceSyntax,
};
use super::{RequestHandler, SourceFileExecution};
use crate::handler::ConfigLoadingGuard;
use crate::{ConfigFileSelection, ConfigLoadOptions};

const STARTUP_CONFIG_ERROR_LIMIT: usize = 256;

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn load_startup_config(&self, config_load: ConfigLoadOptions) {
        let guard = self.start_config_loading();
        self.load_startup_config_with_guard(config_load, guard)
            .await;
    }

    pub(crate) async fn load_startup_config_with_guard(
        &self,
        config_load: ConfigLoadOptions,
        guard: ConfigLoadingGuard,
    ) {
        // A startup that returns before any configuration runs still owns the
        // guard, so it finishes here. Every startup path then reaches the same
        // finaliser exactly once, whichever way it ended.
        if let Some(guard) = self.run_startup_config(config_load, guard).await {
            self.finish_startup_config(guard).await;
        }
    }

    /// Returns the guard when startup ended before `execute_startup_source_file`
    /// took ownership of it, and `None` once that call owns it.
    async fn run_startup_config(
        &self,
        config_load: ConfigLoadOptions,
        guard: ConfigLoadingGuard,
    ) -> Option<ConfigLoadingGuard> {
        if matches!(config_load.selection(), ConfigFileSelection::Disabled) {
            return Some(guard);
        }
        let resolved = startup_config_paths(config_load.selection());
        let (paths, tmux_fallback_paths) = resolved.source_paths();
        self.record_startup_config_files(&paths).await;

        let command = ParsedSourceFileCommand {
            paths,
            quiet: config_load.quiet(),
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            target_missing_canfail: false,
            caller_cwd: config_load.cwd().map(Path::to_path_buf),
            stdin: None,
            current_file: None,
            syntax: SourceSyntax::Rmux,
        };

        let loaded = match self.load_startup_source_file_command(&command, 1).await {
            Ok(loaded) => loaded,
            Err(error) => {
                self.record_config_error(error).await;
                return Some(guard);
            }
        };

        let mut loaded = if resolved.should_load_fallback(loaded.outcome()) {
            self.record_startup_config_files(&tmux_fallback_paths).await;
            let fallback_command = ParsedSourceFileCommand {
                paths: tmux_fallback_paths,
                quiet: true,
                syntax: SourceSyntax::TmuxCompat,
                ..command.clone()
            };
            match self
                .load_startup_source_file_command(&fallback_command, 1)
                .await
            {
                Ok(loaded) => loaded,
                // INTENTIONAL: the tmux fallback is a compatibility import for
                // users who do not have an RMUX config. If it cannot even be
                // loaded, startup must continue like tmux's absent/optional
                // fallback paths rather than failing daemon readiness.
                Err(_) => return Some(guard),
            }
        } else {
            loaded
        };

        let mut errors = Vec::new();
        if let Some(error) = loaded.take_error() {
            errors.push(error);
        }
        let execution = self
            .execute_startup_source_file(
                loaded,
                QueueExecutionContext::new(command.caller_cwd.clone()).for_startup_config(),
                guard,
            )
            .await;
        if let Some(error) = execution.source_file_error {
            errors.push(error);
        }
        errors.extend(
            execution
                .execution_errors
                .into_iter()
                .map(|error| error.into_error()),
        );
        if let Some(error) = super::super::aggregate_rmux_errors(errors) {
            self.record_config_error(error).await;
        }
        None
    }

    /// Runs once at the end of the initial configuration, before the loading
    /// guard is released, so the value the operator configured is the one
    /// consumed and no later `source-file` repeats the load.
    async fn finish_startup_config(&self, guard: ConfigLoadingGuard) {
        self.load_prompt_history_file().await;
        drop(guard);
    }

    async fn record_startup_config_files(&self, paths: &[String]) {
        self.state.lock().await.set_startup_config_files(paths);
    }

    async fn execute_startup_source_file(
        &self,
        loaded: LoadedSourceFile,
        context: QueueExecutionContext,
        guard: ConfigLoadingGuard,
    ) -> SourceFileExecution {
        let result = self
            .execute_loaded_source_file(std::process::id(), loaded, context, 1)
            .await;
        self.finish_startup_config(guard).await;
        result
    }

    async fn record_config_error(&self, error: RmuxError) {
        self.log_config_error_messages(&error).await;
        let mut errors = self.startup_config_errors.lock().await;
        if errors.len() >= STARTUP_CONFIG_ERROR_LIMIT {
            errors.remove(0);
        }
        errors.push(error);
    }
}
