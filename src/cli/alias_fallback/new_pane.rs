//! FNP1 has no legacy mutation route and no replay after an execution offer.
use super::super::ExitFailure;
#[cfg(all(test, unix))]
#[path = "new_pane_tests.rs"]
mod tests;

pub(super) fn run_legacy_checked(
    arguments: &[String],
    socket: &Path,
    connection: &mut Connection,
) -> Result<i32, ExitFailure> {
    let aliases = super::server_command_alias_definitions(connection)?;
    run_legacy_checked_with_aliases(arguments, socket, connection, aliases)
}

pub(super) fn run_legacy_checked_with_aliases(
    arguments: &[String],
    socket: &Path,
    connection: &mut Connection,
    aliases: Vec<String>,
) -> Result<i32, ExitFailure> {
    let parsed = CommandParser::new()
        .with_command_aliases(aliases)
        .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
        .parse_arguments_with_assignments(arguments)
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;
    if parsed
        .commands()
        .iter()
        .any(|command| command.name() == "new-pane")
    {
        return Err(ExitFailure::new(
            1,
            "new-pane requires negotiated execution; no legacy fallback",
        ));
    }
    if parsed
        .commands()
        .iter()
        .any(|command| command.name() == "set-pane-placement")
    {
        return Err(ExitFailure::new(
            1,
            "set-pane-placement requires negotiated execution; no legacy fallback",
        ));
    }
    if parsed
        .commands()
        .iter()
        .any(super::resize_pane_position::requests_position)
    {
        return Err(ExitFailure::new(
            1,
            "resize-pane -X/-Y requires negotiated execution; no legacy fallback",
        ));
    }
    let queue_command = arguments
        .iter()
        .map(|argument| super::tmux_quote_argument(argument))
        .collect::<Vec<_>>()
        .join(" ");
    super::super::command_runner::run_queued_server_command_with_connection(
        connection,
        socket,
        "source-file",
        queue_command,
    )
    .map_err(super::normalize_alias_fallback_error)
}
use rmux_client::Connection;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{
    NewPaneInputV1Request, NewPaneInputV1Response, Request, Response,
    RuntimeCommandExecutionResponse, RuntimeCommandExecutionV2Request, RuntimeCommandIntent,
};
use std::io::{Read, Write};
use std::path::Path;

pub(super) fn run_expanded(
    connection: &mut Connection,
    command: &rmux_core::command_parser::ParsedCommand,
    original: Vec<String>,
) -> Result<i32, ExitFailure> {
    execute(connection, original, Route::of(command))
}

/// Which offer an invocation takes, decided once on the canonical command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// An ordinary `new-pane`, through the runtime execution offer.
    Offer,
    /// `new-pane -I`, through the offer that carries the finished input.
    WithInput,
}

impl Route {
    fn of(command: &rmux_core::command_parser::ParsedCommand) -> Self {
        if rmux_core::cli_entry::new_pane_requires_input(command) {
            Self::WithInput
        } else {
            Self::Offer
        }
    }

    /// Decides the route for argv that already name the command themselves.
    fn for_arguments(arguments: &[String]) -> Result<Self, ExitFailure> {
        let parsed = CommandParser::new()
            .with_command_aliases(std::iter::empty::<String>())
            .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
            .parse_arguments_with_assignments(arguments)
            .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        let [command] = parsed.commands() else {
            return Err(batch_error());
        };
        Ok(Self::of(command))
    }
}

pub(in crate::cli) fn batch_error() -> ExitFailure {
    ExitFailure::new(
        1,
        "CLI batches containing new-pane are not admitted; no commands executed",
    )
}

pub(in crate::cli) fn run(socket: &Path, arguments: Vec<String>) -> Result<i32, ExitFailure> {
    let mut connection = rmux_client::connect(socket)
        .map_err(|error| ExitFailure::from_client_connect(socket, error))?;
    let arguments: Vec<String> = std::iter::once("new-pane".to_owned())
        .chain(arguments)
        .collect();
    // This entry is reached with the command named literally, so the route is
    // decided from these argv; the alias entry decides it before it gets here.
    let route = Route::for_arguments(&arguments)?;
    execute(&mut connection, arguments, route)
}

fn execute(
    connection: &mut Connection,
    arguments: Vec<String>,
    route: Route,
) -> Result<i32, ExitFailure> {
    if route == Route::WithInput {
        return execute_with_input(connection, arguments);
    }
    let offer = RuntimeCommandExecutionV2Request::new(
        arguments,
        std::env::current_dir().ok(),
        RuntimeCommandIntent::NewFloatingPane,
    );
    match connection
        .runtime_command_execution_v2(offer)
        .map_err(ExitFailure::from_client)?
    {
        Response::RuntimeCommandExecution(result) => match *result {
            RuntimeCommandExecutionResponse::Complete {
                stdout,
                error,
                exit_status,
                ..
            } => {
                std::io::stdout()
                    .lock()
                    .write_all(&stdout)
                    .map_err(|error| ExitFailure::new(1, error.to_string()))?;
                match error {
                    Some(error) => Err(ExitFailure::new(exit_status, error.to_string())),
                    None => Ok(exit_status),
                }
            }
            RuntimeCommandExecutionResponse::Unsupported(error) => {
                Err(ExitFailure::new(1, error.to_string()))
            }
            RuntimeCommandExecutionResponse::Fallback => Err(ExitFailure::new(
                1,
                "new-pane offer not admitted; no fallback",
            )),
        },
        Response::Error(error) => Err(ExitFailure::new(1, error.error.to_string())),
        _ => Err(ExitFailure::new(
            1,
            "invalid new-pane offer response; no replay",
        )),
    }
}

/// Reads a finished input to its end, stopping one byte past what can be sent.
///
/// Reading without a bound would hold a whole producer in memory before the
/// frame limit ever refused it. One byte past the maximum is enough to know
/// the payload cannot be sent, and it is also the point where reading stops:
/// what is refused is never fully read. An end of file without a byte is a
/// real payload here, `Some(empty)`, and not the absence of a channel.
fn read_finished_input(source: impl Read) -> Result<Vec<u8>, ExitFailure> {
    let maximum = rmux_proto::DEFAULT_MAX_DETACHED_FRAME_LENGTH;
    let mut payload = Vec::new();
    let read = source
        .take(maximum as u64 + 1)
        .read_to_end(&mut payload)
        .map_err(|error| ExitFailure::new(1, format!("new-pane: failed to read stdin: {error}")))?;
    if read > maximum {
        return Err(ExitFailure::new(
            1,
            format!(
                "new-pane: input not sent, it is larger than the {maximum} bytes one frame                  carries; nothing was created and nothing is retried"
            ),
        ));
    }
    Ok(payload)
}

/// Offers the command and the finished bytes together, once, with no fallback.
fn execute_with_input(
    connection: &mut Connection,
    arguments: Vec<String>,
) -> Result<i32, ExitFailure> {
    let payload = read_finished_input(std::io::stdin().lock())?;
    let offer = NewPaneInputV1Request::new(arguments, std::env::current_dir().ok(), Some(payload));
    // The reader above bounds what is held in memory; this bounds what is sent.
    // The whole envelope has to fit, not the payload alone: argv, the working
    // directory and the handshake travel with it. Refused here, before the
    // server is asked, and never split into a second attempt.
    if let Err(error) = rmux_proto::encode_frame(&Request::NewPaneInputV1(Box::new(offer.clone())))
    {
        return Err(ExitFailure::new(
            1,
            format!("new-pane: input not sent, the offer does not fit one frame: {error}"),
        ));
    }
    match connection
        .new_pane_input_v1(offer)
        .map_err(ExitFailure::from_client)?
    {
        Response::NewPaneInputV1(result) => match *result {
            NewPaneInputV1Response::NotAdmitted { error } => Err(ExitFailure::new(
                1,
                format!("new-pane: {error}; nothing was created and nothing is retried"),
            )),
            NewPaneInputV1Response::Complete {
                stdout,
                error,
                exit_status,
            } => {
                std::io::stdout()
                    .lock()
                    .write_all(&stdout)
                    .map_err(|error| ExitFailure::new(1, error.to_string()))?;
                match error {
                    Some(error) => Err(ExitFailure::new(exit_status.max(1), error.to_string())),
                    None => Ok(exit_status),
                }
            }
        },
        Response::Error(error) => Err(ExitFailure::new(1, error.error.to_string())),
        _ => Err(ExitFailure::new(
            1,
            "invalid new-pane input offer response; no replay",
        )),
    }
}

#[cfg(test)]
mod route_tests {
    use super::{read_finished_input, Route};
    use std::io::Read;

    /// A producer larger than one frame that counts what was asked of it.
    ///
    /// It ends on its own so an unbounded reader still terminates - and then
    /// shows, by its count, that it was drained past the bound before anything
    /// refused it.
    struct Oversized {
        remaining: usize,
        served: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Read for Oversized {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = buffer.len().min(self.remaining);
            buffer[..count].fill(b'x');
            self.remaining -= count;
            self.served.set(self.served.get() + count);
            Ok(count)
        }
    }

    /// The reader stops one byte past what a frame can carry, refuses there,
    /// and never asks for more than that.
    #[test]
    fn fstdin1_the_reader_stops_one_byte_past_what_can_be_sent() {
        let maximum = rmux_proto::DEFAULT_MAX_DETACHED_FRAME_LENGTH;
        let served = std::rc::Rc::new(std::cell::Cell::new(0));
        let error = read_finished_input(Oversized {
            remaining: maximum + 4096,
            served: std::rc::Rc::clone(&served),
        })
        .expect_err("a payload larger than one frame is refused before the offer");
        assert!(
            error.message().contains("larger than"),
            "{}",
            error.message()
        );
        assert!(
            served.get() <= maximum + 1,
            "the reader asked for {} bytes, past the bound",
            served.get()
        );
    }

    /// A finished input is admitted whole, and an end of file without a byte
    /// is a payload of its own.
    #[test]
    fn fstdin1_a_finished_input_is_read_whole_and_an_empty_one_stays_a_payload() {
        assert_eq!(
            read_finished_input(&b"alpha\nbeta\n"[..]).unwrap(),
            b"alpha\nbeta\n"
        );
        assert_eq!(read_finished_input(&b""[..]).unwrap(), Vec::<u8>::new());
    }

    fn route(arguments: &[&str]) -> Route {
        let arguments: Vec<String> = arguments.iter().map(|value| (*value).to_owned()).collect();
        Route::for_arguments(&arguments).expect("these argv name one command")
    }

    /// The decision is on the command, never on the text of a value or of the
    /// process tail, and an alias is decided before these argv are seen.
    #[test]
    fn fstdin1_the_route_reads_the_flag_and_not_a_value_or_the_tail() {
        assert_eq!(route(&["new-pane", "-I"]), Route::WithInput);
        assert_eq!(route(&["new-pane", "-dI"]), Route::WithInput);
        assert_eq!(route(&["newp", "-I", "-d"]), Route::WithInput);
        assert_eq!(route(&["new-pane", "-d"]), Route::Offer);
        assert_eq!(route(&["new-pane", "-F", "-I"]), Route::Offer);
        assert_eq!(route(&["new-pane", "--", "-I"]), Route::Offer);
        assert_eq!(route(&["newp", "-x", "-I", "-d"]), Route::Offer);
    }
}
