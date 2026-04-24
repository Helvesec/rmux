use super::parse;
use clap::{ArgAction, CommandFactory};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

const FROZEN_TMUX_REFERENCE: &str = "tests/reference/tmux_compat/frozen_reference.yaml";
const ERROR_EXIT_MATRIX: &str = "tests/reference/tmux_compat/error_exit_matrix.yaml";

fn parse_args(args: &[&str]) -> Result<super::Cli, clap::Error> {
    let mut full_args = vec!["rmux"];
    full_args.extend_from_slice(args);
    parse(full_args)
}

fn target_text(target: &Option<super::TargetSpec>) -> String {
    target.as_ref().expect("target").to_string()
}

fn repo_file(path: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn structured_section<'a>(contents: &'a str, heading: &str, next_heading: &str) -> &'a str {
    contents
        .split_once(heading)
        .and_then(|(_, tail)| tail.split_once(next_heading).map(|(section, _)| section))
        .unwrap_or_else(|| panic!("missing markdown section {heading}"))
}

fn troff_section<'a>(contents: &'a str, heading: &str, next_heading: &str) -> &'a str {
    contents
        .split_once(heading)
        .and_then(|(_, tail)| tail.split_once(next_heading).map(|(section, _)| section))
        .unwrap_or_else(|| panic!("missing troff section {heading}"))
}

fn troff_literal_block(contents: &str, heading: &str, next_heading: &str) -> Vec<String> {
    troff_section(contents, heading, next_heading)
        .split_once(".nf\n")
        .and_then(|(_, tail)| tail.split_once("\n.fi").map(|(block, _)| block))
        .unwrap_or_else(|| panic!("missing literal block under {heading}"))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn extract_backtick_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = line;

    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let end = rest
            .find('`')
            .unwrap_or_else(|| panic!("unclosed backtick token in {line:?}"));
        tokens.push(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }

    tokens
}

fn ledger_entry_block<'a>(contents: &'a str, id: &str) -> &'a str {
    let needle = format!("  - id: \"{id}\"");
    let start = contents
        .find(&needle)
        .unwrap_or_else(|| panic!("missing ledger entry {id}"));
    let tail = &contents[start..];
    let end = tail[needle.len()..]
        .find("\n  - id: \"")
        .map(|offset| needle.len() + offset)
        .unwrap_or(tail.len());
    &tail[..end]
}

fn ledger_entry_ids(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|line| {
            line.strip_prefix("  - id: \"")
                .and_then(|rest| rest.strip_suffix('"'))
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn ledger_metadata_usize(contents: &str, key: &str) -> usize {
    let prefix = format!("  {key}: ");
    contents
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("missing ledger metadata field {key}"))
        .parse()
        .unwrap_or_else(|error| panic!("invalid numeric ledger metadata field {key}: {error}"))
}

fn rendered_surface_entry(entry: &rmux_core::command_parser::CommandEntry) -> String {
    match entry.alias {
        Some(alias) => format!("{} ({alias})", entry.name),
        None => entry.name.to_owned(),
    }
}

fn help_dispatch_is_supported(name: &str) -> bool {
    let parsed = super::TmuxCommandParser::new()
        .parse(&format!("{name} --help"))
        .unwrap_or_else(|error| panic!("failed to parse help probe for {name}: {error}"))
        .into_commands()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("missing parsed command for {name}"));

    match super::command_from_parsed(parsed) {
        Ok(super::Command::Unsupported(_)) => false,
        Ok(_) => true,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => true,
        Err(error) => panic!("{name} --help failed dispatch classification: {error}"),
    }
}

#[path = "cli_args_tests/session_and_top_level.rs"]
mod session_and_top_level;

#[path = "cli_args_tests/window_commands.rs"]
mod window_commands;

#[path = "cli_args_tests/overlays_and_prompts.rs"]
mod overlays_and_prompts;

#[path = "cli_args_tests/surface_docs.rs"]
mod surface_docs;

#[path = "cli_args_tests/ledger.rs"]
mod ledger;

#[path = "cli_args_tests/compat_reference.rs"]
mod compat_reference;

#[path = "cli_args_tests/queue_and_window_ops.rs"]
mod queue_and_window_ops;

#[path = "cli_args_tests/pane_layout.rs"]
mod pane_layout;

#[path = "cli_args_tests/pane_io.rs"]
mod pane_io;

#[path = "cli_args_tests/scripting_and_buffers.rs"]
mod scripting_and_buffers;

#[path = "cli_args_tests/options_and_scopes.rs"]
mod options_and_scopes;

#[path = "cli_args_tests/server_lifecycle.rs"]
mod server_lifecycle;
