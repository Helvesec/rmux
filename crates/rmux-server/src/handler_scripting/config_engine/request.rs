use std::path::PathBuf;

use rmux_proto::Target;

use super::super::source_files::{ParsedSourceFileCommand, SourceSyntax};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigLoadMode {
    Execute,
    ParseOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigReadPolicy {
    Strict,
    ImportCompat,
}

pub(crate) struct ConfigLoadRequest<'a> {
    pub(super) command: &'a ParsedSourceFileCommand,
    pub(super) syntax: SourceSyntax,
    pub(super) mode: ConfigLoadMode,
    pub(super) read_policy: ConfigReadPolicy,
    pub(super) quiet: bool,
    pub(super) verbose: bool,
    pub(super) caller_cwd: Option<PathBuf>,
    pub(super) current_file: Option<String>,
    pub(super) current_target: Option<Target>,
    pub(super) explicit_target: bool,
    pub(super) implicit_target_refresh: bool,
    pub(super) depth: usize,
}

impl<'a> ConfigLoadRequest<'a> {
    pub(crate) fn from_source_command(
        command: &'a ParsedSourceFileCommand,
        explicit_target: bool,
        implicit_target_refresh: bool,
        depth: usize,
    ) -> Self {
        Self {
            command,
            syntax: command.syntax,
            mode: if command.parse_only {
                ConfigLoadMode::ParseOnly
            } else {
                ConfigLoadMode::Execute
            },
            read_policy: match command.syntax {
                SourceSyntax::Rmux | SourceSyntax::Canonical => ConfigReadPolicy::Strict,
                SourceSyntax::TmuxCompat => ConfigReadPolicy::ImportCompat,
            },
            quiet: command.quiet,
            verbose: command.verbose,
            caller_cwd: command.caller_cwd.clone(),
            current_file: command.current_file.clone(),
            current_target: command.target.clone().map(Target::Pane),
            explicit_target,
            implicit_target_refresh,
            depth,
        }
    }

    pub(crate) fn is_import_compat(&self) -> bool {
        self.read_policy == ConfigReadPolicy::ImportCompat
    }

    pub(crate) fn assert_boundary_invariants(&self) {
        debug_assert_eq!(self.syntax, self.command.syntax);
        debug_assert_eq!(self.quiet, self.command.quiet);
        debug_assert_eq!(self.verbose, self.command.verbose);
        debug_assert_eq!(self.caller_cwd, self.command.caller_cwd);
        debug_assert_eq!(self.current_file, self.command.current_file);
        debug_assert_eq!(
            self.current_target,
            self.command.target.clone().map(Target::Pane)
        );
        debug_assert_eq!(
            self.mode == ConfigLoadMode::ParseOnly,
            self.command.parse_only
        );
        debug_assert_eq!(
            self.read_policy == ConfigReadPolicy::ImportCompat,
            self.syntax == SourceSyntax::TmuxCompat
        );
        debug_assert!(
            !(self.explicit_target && self.implicit_target_refresh),
            "explicit target and implicit target refresh are mutually exclusive"
        );
        let _ = self.is_import_compat();
    }
}
