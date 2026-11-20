use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rmux_core::command_parser::{CommandParser, ParsedCommands};

use super::*;
use crate::{ConfigLoadOptions, DaemonConfig};

#[path = "startup_shutdown_tests.rs"]
mod startup_shutdown_tests;

fn unique_temp_root(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("rmux-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&root).expect("test root directory");
    root
}

fn parse_command(command: &str) -> ParsedCommands {
    CommandParser::new()
        .parse_one_group(command)
        .expect("command parses")
}

/// Runs a command the way an operator does, so the tests drive the same entry
/// paths that can change the option or the store.
async fn run_command(handler: &RequestHandler, command: &str) {
    handler
        .execute_parsed_commands_for_test(std::process::id(), parse_command(command))
        .await
        .expect("command succeeds");
}

async fn set_history_file(handler: &RequestHandler, value: &str) {
    run_command(handler, &format!("set-option -s history-file '{value}'")).await;
}

async fn stored_entries(handler: &RequestHandler) -> HistoryEntries {
    handler.prompt_history.lock().await.export()
}

async fn push_entry(handler: &RequestHandler, prompt_type: PromptType, entry: &str) {
    handler
        .prompt_history
        .lock()
        .await
        .push(prompt_type, entry, 100);
}

fn entry(prompt_type: PromptType, value: &str) -> (PromptType, String) {
    (prompt_type, value.to_owned())
}

#[test]
fn encode_renders_type_prefixed_lines_in_canonical_order() {
    let entries = vec![
        entry(PromptType::Command, "kill-pane"),
        entry(PromptType::Search, "needle"),
        entry(PromptType::Target, "%3"),
        entry(PromptType::WindowTarget, "alpha:1"),
    ];

    assert_eq!(
        encode(&entries),
        "command:kill-pane\nsearch:needle\ntarget:%3\nwindow-target:alpha:1\n"
    );
}

#[test]
fn decode_splits_on_the_first_colon_only() {
    assert_eq!(
        decode("command:display-message -p -- 'a:b:c'\n"),
        vec![entry(PromptType::Command, "display-message -p -- 'a:b:c'")]
    );
}

#[test]
fn decode_keeps_an_unknown_or_absent_prefix_whole_as_a_command() {
    assert_eq!(
        decode("bogus:value\nno-colon-at-all\n"),
        vec![
            entry(PromptType::Command, "bogus:value"),
            entry(PromptType::Command, "no-colon-at-all"),
        ]
    );
}

#[test]
fn decode_accepts_a_missing_final_newline_and_strips_only_that_one() {
    assert_eq!(
        decode("command:one"),
        vec![entry(PromptType::Command, "one")]
    );
    assert_eq!(
        decode("command:one\n\n"),
        vec![
            entry(PromptType::Command, "one"),
            entry(PromptType::Command, ""),
        ]
    );
}

#[test]
fn a_file_with_no_bytes_holds_no_entry() {
    assert!(decode("").is_empty());
}

/// tmux reads the length-1 line, drops the terminator and imports the empty
/// entry (`status.c:126-138`, accepted by `status.c:1789`), so the file holds
/// one empty `Command` and not nothing.
#[test]
fn a_file_holding_only_the_terminator_holds_one_empty_command() {
    assert_eq!(decode("\n"), vec![entry(PromptType::Command, "")]);
    assert_eq!(encode(&decode("\n")), "command:\n");
}

/// Two terminators are two empty lines before the store deduplicates them; that
/// is what separates this case from the empty file.
#[test]
fn two_terminators_decode_to_two_empty_commands() {
    assert_eq!(
        decode("\n\n"),
        vec![
            entry(PromptType::Command, ""),
            entry(PromptType::Command, ""),
        ]
    );
}

#[test]
fn encode_and_decode_round_trip_every_prompt_type() {
    let entries = vec![
        entry(PromptType::Command, "first"),
        entry(PromptType::Command, "second"),
        entry(PromptType::Search, "needle"),
        entry(PromptType::Target, "%1"),
        entry(PromptType::WindowTarget, "alpha:2"),
    ];

    assert_eq!(decode(&encode(&entries)), entries);
}

#[test]
fn an_empty_option_resolves_to_no_io() {
    assert_eq!(resolve_path_from("", Some(Path::new("/home/tester"))), None);
}

#[test]
fn a_relative_value_is_ignored() {
    assert_eq!(
        resolve_path_from("history", Some(Path::new("/home/tester"))),
        None
    );
    assert_eq!(
        resolve_path_from("./history", Some(Path::new("/home/tester"))),
        None
    );
}

#[cfg(unix)]
#[test]
fn an_absolute_value_is_used_verbatim() {
    assert_eq!(
        resolve_path_from("/var/tmp/history", None),
        Some(PathBuf::from("/var/tmp/history"))
    );
}

#[cfg(windows)]
#[test]
fn windows_accepts_drive_and_unc_paths_but_not_drive_relative_ones() {
    assert_eq!(
        resolve_path_from(r"C:\rmux\history", None),
        Some(PathBuf::from(r"C:\rmux\history"))
    );
    assert_eq!(
        resolve_path_from(r"\\server\share\history", None),
        Some(PathBuf::from(r"\\server\share\history"))
    );
    assert_eq!(resolve_path_from(r"\rmux\history", None), None);
    assert_eq!(resolve_path_from("/rmux/history", None), None);
    // Relative to the current directory of a drive, which the platform does not
    // call absolute either.
    assert_eq!(resolve_path_from("C:history", None), None);
    assert_eq!(resolve_path_from(r"C:history\prompt", None), None);
}

#[test]
fn a_tilde_value_resolves_against_the_daemon_home() {
    assert_eq!(
        resolve_path_from("~/history", Some(Path::new("/home/tester"))),
        Some(PathBuf::from("/home/tester/history"))
    );
}

/// The suffix after `~` is concatenated, never joined: a doubled separator is
/// still part of the home's path, exactly as tmux's `"%s%s"` builds it
/// (`status.c:80`). Joining would resolve to `/history` and read, create or
/// truncate a file the operator never named.
#[test]
fn a_doubled_separator_after_the_tilde_keeps_the_daemon_home() {
    assert_eq!(
        resolve_path_from("~//history", Some(Path::new("/home/tester"))),
        Some(PathBuf::from("/home/tester//history"))
    );
    assert_eq!(
        resolve_path_from("~///history", Some(Path::new("/home/tester"))),
        Some(PathBuf::from("/home/tester///history"))
    );
}

#[cfg(windows)]
#[test]
fn a_separator_suffix_after_the_tilde_keeps_the_windows_home_and_drive() {
    let home = Some(Path::new(r"C:\Users\tester"));
    assert_eq!(
        resolve_path_from(r"~\history", home),
        Some(PathBuf::from(r"C:\Users\tester\history"))
    );
    assert_eq!(
        resolve_path_from(r"~\\history", home),
        Some(PathBuf::from(r"C:\Users\tester\\history"))
    );
    assert_eq!(
        resolve_path_from("~//history", home),
        Some(PathBuf::from(r"C:\Users\tester//history"))
    );
}

#[test]
fn a_bare_tilde_or_tilde_user_is_not_expanded() {
    let home = Some(Path::new("/home/tester"));
    assert_eq!(resolve_path_from("~", home), None);
    assert_eq!(resolve_path_from("~other/history", home), None);
}

#[test]
fn a_tilde_value_without_a_home_does_no_io() {
    assert_eq!(resolve_path_from("~/history", None), None);
}

#[tokio::test]
async fn startup_config_load_consumes_the_file_that_config_configured() {
    let root = unique_temp_root("history-file-startup");
    let history = root.join("history");
    fs::write(&history, "command:typed-at-the-prompt\nsearch:needle\n").expect("history file");
    let config = root.join("rmux.conf");
    fs::write(
        &config,
        format!("set -s history-file '{}'\n", history.display()),
    )
    .expect("config file");

    let handler = RequestHandler::new();
    let daemon = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
        vec![config],
        false,
        Some(root.clone()),
    );
    handler
        .load_startup_config(daemon.config_load().clone())
        .await;

    assert_eq!(
        stored_entries(&handler).await,
        vec![
            entry(PromptType::Command, "typed-at-the-prompt"),
            entry(PromptType::Search, "needle"),
        ]
    );
}

#[tokio::test]
async fn startup_without_config_files_still_consumes_the_history_file() {
    let root = unique_temp_root("history-file-disabled");
    let history = root.join("history");
    fs::write(&history, "command:from-disabled-startup\n").expect("history file");

    let handler = RequestHandler::new();
    set_history_file(&handler, &history.display().to_string()).await;
    handler
        .load_startup_config(ConfigLoadOptions::disabled())
        .await;

    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "from-disabled-startup")]
    );
}

#[tokio::test]
async fn a_failing_config_command_does_not_suppress_the_history_load() {
    let root = unique_temp_root("history-file-config-error");
    let history = root.join("history");
    fs::write(&history, "command:survives-a-config-error\n").expect("history file");
    let config = root.join("rmux.conf");
    fs::write(
        &config,
        format!(
            "set -s history-file '{}'\nkill-session -t no-such-session\n",
            history.display()
        ),
    )
    .expect("config file");

    let handler = RequestHandler::new();
    let daemon = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
        vec![config],
        false,
        Some(root.clone()),
    );
    handler
        .load_startup_config(daemon.config_load().clone())
        .await;

    assert!(
        !handler.startup_config_errors.lock().await.is_empty(),
        "the config must have reported its error"
    );
    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "survives-a-config-error")]
    );
    push_entry(&handler, PromptType::Command, "typed-after-error").await;
    handler.save_prompt_history_file().await;
    assert_eq!(
        fs::read_to_string(&history).unwrap(),
        "command:survives-a-config-error\ncommand:typed-after-error\n"
    );
}

#[tokio::test]
async fn an_unloadable_config_file_does_not_suppress_the_history_load() {
    let root = unique_temp_root("history-file-config-unloadable");
    let history = root.join("history");
    fs::write(&history, "command:survives-an-unloadable-config\n").expect("history file");
    let unloadable = root.join("config-as-a-directory");
    fs::create_dir(&unloadable).expect("directory in place of the config file");

    let handler = RequestHandler::new();
    set_history_file(&handler, &history.display().to_string()).await;
    let daemon = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
        vec![unloadable],
        false,
        Some(root.clone()),
    );
    handler
        .load_startup_config(daemon.config_load().clone())
        .await;

    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "survives-an-unloadable-config")]
    );
    push_entry(&handler, PromptType::Command, "typed-after-load-error").await;
    handler.save_prompt_history_file().await;
    assert_eq!(
        fs::read_to_string(&history).unwrap(),
        "command:survives-an-unloadable-config\ncommand:typed-after-load-error\n"
    );
}

#[tokio::test]
async fn a_stop_writes_the_history_the_next_daemon_recalls() {
    let root = unique_temp_root("history-file-round-trip");
    let history = root.join("history");

    let stopping = RequestHandler::new();
    stopping
        .load_startup_config(ConfigLoadOptions::disabled())
        .await;
    set_history_file(&stopping, &history.display().to_string()).await;
    push_entry(&stopping, PromptType::Command, "nonce-command").await;
    push_entry(&stopping, PromptType::Search, "nonce-search").await;
    stopping.save_prompt_history_file().await;

    assert_eq!(
        fs::read_to_string(&history).expect("history file written"),
        "command:nonce-command\nsearch:nonce-search\n"
    );

    let restarted = RequestHandler::new();
    set_history_file(&restarted, &history.display().to_string()).await;
    restarted.load_prompt_history_file().await;

    assert_eq!(
        restarted
            .show_prompt_history(None)
            .await
            .expect("show-prompt-history succeeds"),
        "History for command:\n\n1: nonce-command\n\nHistory for search:\n\n1: nonce-search\n\nHistory for target:\n\n\nHistory for window-target:\n\n\n"
    );
}

#[tokio::test]
async fn loading_applies_the_current_prompt_history_limit() {
    let root = unique_temp_root("history-file-limit");
    let history = root.join("history");
    fs::write(&history, "command:one\ncommand:two\ncommand:three\n").expect("history file");

    let handler = RequestHandler::new();
    run_command(&handler, "set-option -s prompt-history-limit 2").await;
    set_history_file(&handler, &history.display().to_string()).await;
    handler.load_prompt_history_file().await;

    assert_eq!(
        stored_entries(&handler).await,
        vec![
            entry(PromptType::Command, "two"),
            entry(PromptType::Command, "three"),
        ]
    );
}

#[tokio::test]
async fn clearing_the_option_while_running_disables_the_save() {
    let root = unique_temp_root("history-file-cleared");
    let history = root.join("history");

    let handler = RequestHandler::new();
    handler
        .load_startup_config(ConfigLoadOptions::disabled())
        .await;
    set_history_file(&handler, &history.display().to_string()).await;
    push_entry(&handler, PromptType::Command, "not-persisted").await;
    set_history_file(&handler, "").await;
    handler.save_prompt_history_file().await;

    assert!(!history.exists(), "an empty value must do no I/O");
    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "not-persisted")],
        "the in-memory history stays usable"
    );
}

/// A hot `set-option` or `source-file` only changes where the stop will write.
///
/// B is pre-filled with its own nonce, the switch is made by the two commands
/// that can really change the value at runtime, and nothing here calls the
/// loader by hand: B must still hold its nonce, and the RAM history must still
/// hold A's, until the stop replaces B with the RAM history and leaves A alone.
#[tokio::test]
async fn a_hot_switch_never_reloads_and_only_the_stop_writes_the_new_file() {
    let root = unique_temp_root("history-file-switch");
    let first = root.join("first");
    let second = root.join("second");
    fs::write(&first, "command:loaded-from-first\n").expect("first history file");
    fs::write(&second, "command:SHOULD-NOT-RELOAD\n").expect("second history file");
    let config = root.join("rmux.conf");
    fs::write(
        &config,
        format!("set -s history-file '{}'\n", first.display()),
    )
    .expect("config file");

    // Startup is the only load: the configuration names A and the finaliser
    // consumes it.
    let handler = RequestHandler::new();
    let daemon = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
        vec![config],
        false,
        Some(root.clone()),
    );
    handler
        .load_startup_config(daemon.config_load().clone())
        .await;
    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "loaded-from-first")]
    );

    set_history_file(&handler, &second.display().to_string()).await;
    let hot = root.join("hot.conf");
    fs::write(
        &hot,
        format!("set -s history-file '{}'\n", second.display()),
    )
    .expect("hot config file");
    run_command(&handler, &format!("source-file '{}'", hot.display())).await;

    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "loaded-from-first")],
        "neither set-option nor source-file reloads the newly named file"
    );
    assert_eq!(
        fs::read_to_string(&second).expect("second history file readable"),
        "command:SHOULD-NOT-RELOAD\n",
        "the newly named file is untouched before the stop"
    );

    push_entry(&handler, PromptType::Command, "typed-after-the-switch").await;
    handler.save_prompt_history_file().await;

    assert_eq!(
        fs::read_to_string(&second).expect("second history file written"),
        "command:loaded-from-first\ncommand:typed-after-the-switch\n",
        "the stop writes the RAM history to the current value"
    );
    assert_eq!(
        fs::read_to_string(&first).expect("first history file kept"),
        "command:loaded-from-first\n",
        "the previous file stays untouched"
    );
}

/// The empty entry of a terminator-only file is a real entry: it is imported,
/// rendered and written back as `command:`.
#[tokio::test]
async fn an_empty_entry_is_imported_rendered_and_saved_back() {
    let root = unique_temp_root("history-file-empty-entry");
    let history = root.join("history");
    fs::write(&history, "\n").expect("history file");

    let handler = RequestHandler::new();
    set_history_file(&handler, &history.display().to_string()).await;
    handler.load_prompt_history_file().await;

    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "")]
    );
    assert_eq!(
        handler
            .show_prompt_history(Some(PromptType::Command))
            .await
            .expect("show-prompt-history succeeds"),
        "History for command:\n\n1: \n\n"
    );

    handler.save_prompt_history_file().await;
    assert_eq!(
        fs::read_to_string(&history).expect("history file rewritten"),
        "command:\n"
    );

    // Two terminators are two empty lines in the file and still one entry in
    // the store: import replays them through the store's own consecutive
    // deduplication rather than around it.
    let doubled = root.join("doubled");
    fs::write(&doubled, "\n\n").expect("doubled history file");
    let other = RequestHandler::new();
    set_history_file(&other, &doubled.display().to_string()).await;
    other.load_prompt_history_file().await;
    assert_eq!(
        stored_entries(&other).await,
        vec![entry(PromptType::Command, "")]
    );
}

/// An empty entry counts against the limit like any other loaded line.
#[tokio::test]
async fn the_limit_still_applies_when_the_file_opens_with_an_empty_entry() {
    let root = unique_temp_root("history-file-empty-limit");
    let history = root.join("history");
    fs::write(&history, "\ncommand:one\ncommand:two\n").expect("history file");

    let handler = RequestHandler::new();
    run_command(&handler, "set-option -s prompt-history-limit 2").await;
    set_history_file(&handler, &history.display().to_string()).await;
    handler.load_prompt_history_file().await;

    assert_eq!(
        stored_entries(&handler).await,
        vec![
            entry(PromptType::Command, "one"),
            entry(PromptType::Command, "two"),
        ]
    );
}

#[tokio::test]
async fn a_clear_is_reflected_by_the_next_save() {
    let root = unique_temp_root("history-file-clear");
    let history = root.join("history");

    let handler = RequestHandler::new();
    set_history_file(&handler, &history.display().to_string()).await;
    handler
        .load_startup_config(ConfigLoadOptions::disabled())
        .await;
    push_entry(&handler, PromptType::Command, "kept").await;
    push_entry(&handler, PromptType::Search, "dropped").await;
    handler
        .clear_prompt_history(Some(PromptType::Search))
        .await
        .expect("clear-prompt-history succeeds");
    handler.save_prompt_history_file().await;

    assert_eq!(
        fs::read_to_string(&history).expect("history file written"),
        "command:kept\n"
    );
    handler
        .clear_prompt_history(None)
        .await
        .expect("clear all history");
    handler.save_prompt_history_file().await;
    assert_eq!(
        fs::read(&history).expect("intentionally empty history"),
        b""
    );
}

#[tokio::test]
async fn an_unreadable_history_file_is_not_fatal() {
    let root = unique_temp_root("history-file-unreadable");
    let directory = root.join("history-as-a-directory");
    fs::create_dir(&directory).expect("directory in place of the file");

    let handler = RequestHandler::new();
    set_history_file(&handler, &directory.display().to_string()).await;
    push_entry(&handler, PromptType::Command, "still-here").await;
    handler.load_prompt_history_file().await;
    handler.save_prompt_history_file().await;

    assert!(directory.is_dir(), "the directory is left alone");
    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "still-here")]
    );
    run_command(&handler, "set-option -s prompt-history-limit 50").await;
    let writable = root.join("writable-history");
    set_history_file(&handler, &writable.display().to_string()).await;
    handler.save_prompt_history_file().await;
    assert_eq!(
        fs::read_to_string(writable).unwrap(),
        "command:still-here\n"
    );
}

#[tokio::test]
async fn a_missing_parent_directory_is_not_created() {
    let root = unique_temp_root("history-file-missing-parent");
    let absent = root.join("absent");
    let history = absent.join("history");

    let handler = RequestHandler::new();
    set_history_file(&handler, &history.display().to_string()).await;
    handler.load_prompt_history_file().await;
    push_entry(&handler, PromptType::Command, "typed").await;
    handler.save_prompt_history_file().await;

    assert!(!absent.exists(), "no mkdir is performed");
    assert_eq!(
        stored_entries(&handler).await,
        vec![entry(PromptType::Command, "typed")]
    );
}

/// Replays the exact file the tmux 3.7b oracle was given and asserts the same
/// rendered history: consecutive duplicates collapse, an absent prefix and an
/// unknown one both stay whole as `command`, and the final LF is optional.
///
/// Source: `runs/hfile1/oracle-run-20260912T225740Z/observations.json`,
/// `typed_hot.original_unchanged` in, `typed_hot.initial` out.
#[tokio::test]
async fn a_loaded_file_renders_the_history_the_tmux_oracle_rendered() {
    let root = unique_temp_root("history-file-oracle");
    let history = root.join("history");
    fs::write(
        &history,
        "command:first\ncommand:first\nsearch:needle\ntarget:%0\nwindow-target:@0\nlegacy\nunknown:tail\ncommand:last",
    )
    .expect("history file");

    let handler = RequestHandler::new();
    set_history_file(&handler, &history.display().to_string()).await;
    handler.load_prompt_history_file().await;

    assert_eq!(
        handler
            .show_prompt_history(None)
            .await
            .expect("show-prompt-history succeeds"),
        "History for command:\n\n1: first\n2: legacy\n3: unknown:tail\n4: last\n\nHistory for search:\n\n1: needle\n\nHistory for target:\n\n1: %0\n\nHistory for window-target:\n\n1: @0\n\n"
    );

    handler.save_prompt_history_file().await;
    assert_eq!(
        fs::read_to_string(&history).expect("history file rewritten"),
        "command:first\ncommand:legacy\ncommand:unknown:tail\ncommand:last\nsearch:needle\ntarget:%0\nwindow-target:@0\n",
        "a re-saved file re-tags the lines it accepted whole"
    );
}

/// Source: the same oracle run, `missing.new_file_after` is an empty file.
#[tokio::test]
async fn an_absent_file_with_an_existing_parent_is_created_at_stop() {
    let root = unique_temp_root("history-file-absent");
    let history = root.join("history");

    let handler = RequestHandler::new();
    set_history_file(&handler, &history.display().to_string()).await;
    handler.load_prompt_history_file().await;
    assert!(stored_entries(&handler).await.is_empty());

    handler.save_prompt_history_file().await;
    assert_eq!(
        fs::read_to_string(&history).expect("history file created"),
        "",
        "an empty history still creates the file"
    );
}
