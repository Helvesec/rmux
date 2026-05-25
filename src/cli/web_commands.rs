use std::io::IsTerminal;
use std::path::Path;

use qrcode::render::unicode::Dense1x2;
use rmux_proto::{
    CommandOutput, CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest,
    PaneTargetRef, Response, StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest,
    WebShareCreatedResponse, WebShareRequest, WebShareResponse, WebShareScope, WebShareUrlOptions,
    WebTerminalTheme,
};

use super::{
    connect_with_startserver, finish_command_success, resolve_current_pane_target,
    resolve_pane_target_spec, resolve_session_target_spec,
    terminal_theme::capture_terminal_palette, write_command_output, ExitFailure, StartupOptions,
};
use crate::cli_args::{TargetSpec, WebShareArgs, WebShareTerminalThemeArg};

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

    if args.controls && !args.writable {
        return Err(ExitFailure::new(
            1,
            "web-share --controls requires --writable",
        ));
    }
    let scope = resolve_web_share_scope(connection, args.target.as_ref(), args.controls)?;
    let terminal_theme = args.terminal_theme.map(web_terminal_theme);
    let terminal_palette = match terminal_theme {
        Some(WebTerminalTheme::Light | WebTerminalTheme::Dark) => None,
        Some(WebTerminalTheme::User) | None => capture_terminal_palette(),
    };
    Ok(WebShareRequest::Create(CreateWebShareRequest {
        scope,
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
        terminal_palette: terminal_palette.map(Box::new),
        writable: args.writable,
        controls: args.controls,
    }))
}

fn resolve_web_share_scope(
    connection: &mut rmux_client::Connection,
    target: Option<&TargetSpec>,
    controls: bool,
) -> Result<WebShareScope, ExitFailure> {
    let scope = match target {
        Some(target) => resolve_web_share_target_spec(connection, target)?,
        None => WebShareScope::Pane(PaneTargetRef::slot(resolve_current_pane_target(
            connection,
            "web-share",
        )?)),
    };
    if controls && scope.is_pane() {
        return Err(ExitFailure::new(
            1,
            "web-share --controls requires a session target",
        ));
    }
    Ok(scope)
}

fn resolve_web_share_target_spec(
    connection: &mut rmux_client::Connection,
    target: &TargetSpec,
) -> Result<WebShareScope, ExitFailure> {
    match target.exact() {
        Some(rmux_proto::Target::Session(_)) => {
            let session_name = resolve_session_target_spec(connection, target, false)?;
            Ok(WebShareScope::Session(session_name))
        }
        Some(rmux_proto::Target::Pane(_)) | None => {
            let pane = resolve_pane_target_spec(connection, target)?;
            Ok(WebShareScope::Pane(PaneTargetRef::slot(pane)))
        }
        Some(rmux_proto::Target::Window(_)) => Err(ExitFailure::new(
            1,
            "web-share -t accepts pane or session targets, not window targets",
        )),
    }
}

const fn web_terminal_theme(value: WebShareTerminalThemeArg) -> WebTerminalTheme {
    match value {
        WebShareTerminalThemeArg::User => WebTerminalTheme::User,
        WebShareTerminalThemeArg::Light => WebTerminalTheme::Light,
        WebShareTerminalThemeArg::Dark => WebTerminalTheme::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::read_qr_output;
    use crate::cli_args::parse_target_spec;

    #[test]
    fn read_qr_uses_compact_unicode_blocks() {
        let output =
            read_qr_output("https://share.rmux.io/#t=ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq");
        let qr = std::str::from_utf8(output.stdout()).expect("QR output should be UTF-8");

        assert!(!qr.contains('#'));
        assert!(qr.contains('\u{2580}') || qr.contains('\u{2584}') || qr.contains('\u{2588}'));
        assert!(qr.lines().count() < 40);
    }

    #[test]
    fn web_share_session_target_stays_session_scoped() {
        let target = parse_target_spec("webdemo").expect("session target should parse");

        assert!(matches!(
            target.exact(),
            Some(rmux_proto::Target::Session(_))
        ));
    }

    #[test]
    fn web_share_pane_target_stays_exact() {
        let target = parse_target_spec("webdemo:1.2").expect("pane target should parse");
        assert!(matches!(target.exact(), Some(rmux_proto::Target::Pane(_))));
    }
}
