use std::io::Read;
use std::path::Path;

use super::{run_command, run_payload_command, ExitFailure};
use crate::cli_args::{LoadBufferArgs, SaveBufferArgs};

pub(super) fn run_load_buffer(
    args: LoadBufferArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if args.path == "-" {
        let content = read_stdin_bytes("load-buffer")?;
        return run_command(socket_path, "set-buffer", move |connection| {
            connection.set_buffer(args.name, content, false, None, args.set_clipboard)
        });
    }

    run_command(socket_path, "load-buffer", move |connection| {
        connection.load_buffer(args.path, args.name, args.set_clipboard)
    })
}

pub(super) fn run_save_buffer(
    args: SaveBufferArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if args.path == "-" {
        return run_payload_command(socket_path, "show-buffer", move |connection| {
            connection.show_buffer(args.name)
        });
    }

    run_command(socket_path, "save-buffer", move |connection| {
        connection.save_buffer(args.path, args.name, args.append)
    })
}

fn read_stdin_bytes(command_name: &str) -> Result<Vec<u8>, ExitFailure> {
    let mut content = Vec::new();
    std::io::stdin()
        .read_to_end(&mut content)
        .map_err(|error| {
            ExitFailure::new(1, format!("{command_name}: failed to read stdin: {error}"))
        })?;
    Ok(content)
}
