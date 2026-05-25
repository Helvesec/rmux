use std::borrow::Cow;
use std::io::IsTerminal;
use std::path::Path;

use qrcode::render::unicode::Dense1x2;
use rmux_proto::{
    CommandOutput, CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest,
    PaneTargetRef, Response, StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest,
    WebShareCreatedResponse, WebShareRequest, WebShareResponse, WebShareUrlOptions,
    WebTerminalTheme,
};

use super::{
    connect_with_startserver, finish_command_success, resolve_current_pane_target,
    resolve_pane_target_spec, terminal_theme::capture_terminal_palette, write_command_output,
    ExitFailure, StartupOptions,
};
use crate::cli_args::{parse_target_spec, TargetSpec, WebShareArgs, WebShareTerminalThemeArg};

pub(super) fn run_web_share(
    args: WebShareArgs,
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<i32, ExitFailure> {
    let mut connection = connect_with_startserver(socket_path, startup)?;
    let request = build_web_share_request(args, &mut connection)?;
    let response = connection
        .web_share(request)
        .map_err(ExitFailure::from_client)?;
    warn_operator_url(&response);
    if let Response::WebShare(WebShareResponse::Created(created)) = &response {
        write_created_share_output(created)?;
        return Ok(0);
    }
    finish_command_success(response, "web-share")
}

fn warn_operator_url(response: &Response) {
    let Response::WebShare(WebShareResponse::Created(created)) = response else {
        return;
    };
    let Some(operator_url) = created.operator_url.as_deref() else {
        return;
    };
    eprintln!("rmux: operator URL (writable, keep private):");
    eprintln!("rmux:   {operator_url}");
}

fn write_created_share_output(created: &WebShareCreatedResponse) -> Result<(), ExitFailure> {
    write_command_output(&created.output)?;
    let qr_output = if std::io::stdout().is_terminal() {
        read_qr_output(&created.read_url)
    } else {
        CommandOutput::from_stdout("QR omitted (stdout not a terminal); see URL above\n")
    };
    write_command_output(&qr_output)
}

fn read_qr_output(read_url: &str) -> CommandOutput {
    match qrcode::QrCode::new(read_url.as_bytes()) {
        Ok(code) => {
            let qr = code.render::<Dense1x2>().module_dimensions(1, 1).build();
            CommandOutput::from_stdout(format!("{qr}\n"))
        }
        Err(_) => CommandOutput::from_stdout("QR omitted (read URL is too large)\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::{read_qr_output, web_share_pane_lookup_target};
    use crate::cli_args::parse_target_spec;

    #[test]
    fn read_qr_uses_compact_unicode_blocks() {
        let output = read_qr_output(
            "https://share.rmux.io/#endpoint=ws://127.0.0.1:9777/share&id=abcdefgh&key=ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghi",
        );
        let qr = std::str::from_utf8(output.stdout()).expect("QR output should be UTF-8");

        assert!(!qr.contains('#'));
        assert!(qr.contains('\u{2580}') || qr.contains('\u{2584}') || qr.contains('\u{2588}'));
        assert!(qr.lines().count() < 40);
    }

    #[test]
    fn web_share_session_target_resolves_through_active_pane_lookup() {
        let target = parse_target_spec("webdemo").expect("session target should parse");
        let lookup = web_share_pane_lookup_target(&target).expect("lookup target");

        assert_eq!(lookup.raw(), "webdemo:");
    }

    #[test]
    fn web_share_pane_target_stays_exact() {
        let target = parse_target_spec("webdemo:1.2").expect("pane target should parse");
        let lookup = web_share_pane_lookup_target(&target).expect("lookup target");

        assert_eq!(lookup.raw(), "webdemo:1.2");
    }
}

fn build_web_share_request(
    args: WebShareArgs,
    connection: &mut rmux_client::Connection,
) -> Result<WebShareRequest, ExitFailure> {
    if args.list {
        return Ok(WebShareRequest::List(ListWebSharesRequest));
    }
    if let Some(share_id) = args.stop {
        return Ok(WebShareRequest::Stop(StopWebShareRequest { share_id }));
    }
    if args.stop_all {
        return Ok(WebShareRequest::StopAll(StopAllWebSharesRequest));
    }
    if let Some(share_id) = args.lookup {
        return Ok(WebShareRequest::Lookup(LookupWebShareRequest { share_id }));
    }
    if args.config {
        return Ok(WebShareRequest::Config(WebShareConfigRequest));
    }

    let target = resolve_web_share_target(connection, args.target.as_ref())?;
    let terminal_theme = args.terminal_theme.map(web_terminal_theme);
    let terminal_palette = match terminal_theme {
        Some(WebTerminalTheme::Light | WebTerminalTheme::Dark) => None,
        Some(WebTerminalTheme::User) | None => capture_terminal_palette(),
    };
    Ok(WebShareRequest::Create(CreateWebShareRequest {
        target: PaneTargetRef::slot(target),
        public_base_url: args.public_base_url,
        frontend_url: args.frontend_url,
        ttl_seconds: args.ttl_seconds,
        max_readers: args.max_readers,
        url_options: WebShareUrlOptions {
            no_navbar: args.no_navbar,
            no_disclaimer: args.no_disclaimer,
            terminal_theme,
        },
        require_pin: args.require_pin,
        terminal_palette,
        writable: args.writable,
    }))
}

fn resolve_web_share_target(
    connection: &mut rmux_client::Connection,
    target: Option<&TargetSpec>,
) -> Result<rmux_proto::PaneTarget, ExitFailure> {
    match target {
        Some(target) => {
            let lookup = web_share_pane_lookup_target(target)?;
            resolve_pane_target_spec(connection, lookup.as_ref())
        }
        None => resolve_current_pane_target(connection, "web-share"),
    }
}

fn web_share_pane_lookup_target(target: &TargetSpec) -> Result<Cow<'_, TargetSpec>, ExitFailure> {
    let Some(rmux_proto::Target::Session(session_name)) = target.exact() else {
        return Ok(Cow::Borrowed(target));
    };

    parse_target_spec(&format!("{session_name}:"))
        .map(Cow::Owned)
        .map_err(|error| ExitFailure::new(1, error))
}

const fn web_terminal_theme(value: WebShareTerminalThemeArg) -> WebTerminalTheme {
    match value {
        WebShareTerminalThemeArg::User => WebTerminalTheme::User,
        WebShareTerminalThemeArg::Light => WebTerminalTheme::Light,
        WebShareTerminalThemeArg::Dark => WebTerminalTheme::Dark,
    }
}
