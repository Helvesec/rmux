use std::io::IsTerminal;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use rmux_client::{detect_context, ClientContext};
use rmux_proto::{
    CommandOutput, CreateWebShareRequest, ErrorResponse, ListWebSharesRequest,
    LookupWebShareRequest, PaneTargetRef, Response, SessionName, StopAllWebSharesRequest,
    StopWebShareRequest, WebShareConfigRequest, WebShareCreatedResponse, WebShareRequest,
    WebShareResponse, WebShareScope, WebShareUrlOptions, WebTerminalTheme,
};

use super::web_share_display::created_share_terminal_output;
use super::{
    connect_with_startserver, finish_command_success, resolve_current_pane_target,
    resolve_pane_target_spec, resolve_session_target_spec,
    terminal_theme::capture_terminal_palette, unexpected_response, write_command_output,
    ExitFailure, StartupOptions,
};
use crate::cli_args::{
    TargetSpec, WebShareArgs, WebShareTerminalThemeArg, WEB_SHARE_TUNNEL_PROVIDERS,
};

const AUTO_WEB_SHARE_SESSION_ATTEMPTS: u32 = 128;

pub(super) fn run_web_share(
    args: WebShareArgs,
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<i32, ExitFailure> {
    validate_web_share_args_without_daemon(&args)?;
    let mut connection = connect_with_startserver(socket_path, startup)?;
    let disconnect_output = args.disconnect.is_some();
    let request = build_web_share_request(args, &mut connection)?;
    let response = connection
        .web_share(request)
        .map_err(ExitFailure::from_client)?;
    if let Response::WebShare(WebShareResponse::Created(created)) = &response {
        write_created_share_output(created)?;
        return Ok(0);
    }
    if disconnect_output {
        if let Response::WebShare(WebShareResponse::Stopped(stopped)) = &response {
            write_command_output(&disconnect_share_output(
                stopped.share_id.as_str(),
                stopped.stopped,
            ))?;
            return Ok(0);
        }
    }
    finish_command_success(response, "web-share")
}

fn validate_web_share_args_without_daemon(args: &WebShareArgs) -> Result<(), ExitFailure> {
    if args.tunnel_provider.as_deref() == Some("") {
        return Err(ExitFailure::new(
            1,
            format!(
                "web-share --tunnel-provider requires a provider\nAvailable: {}.",
                WEB_SHARE_TUNNEL_PROVIDERS.join(", ")
            ),
        ));
    }
    Ok(())
}

fn disconnect_share_output(share_id: &str, stopped: bool) -> CommandOutput {
    let status = if stopped { "disconnected" } else { "missing" };
    CommandOutput::from_stdout(format!("{status} {share_id}\n"))
}

fn warn_operator_url(created: &WebShareCreatedResponse) {
    let Some(operator_url) = created.operator_url.as_deref() else {
        return;
    };
    eprintln!("rmux: operator URL (keep private):");
    eprintln!("rmux:   {operator_url}");
}

fn write_created_share_output(created: &WebShareCreatedResponse) -> Result<(), ExitFailure> {
    let output = if std::io::stdout().is_terminal() {
        created_share_terminal_output(created)
    } else {
        warn_operator_url(created);
        created.output.clone()
    };
    write_command_output(&output)
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
    if let Some(share_id) = args.disconnect {
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

    if args.ttl_seconds.is_some() && args.expires_at.is_some() {
        return Err(ExitFailure::new(
            1,
            "web-share --ttl and --expires-at are mutually exclusive",
        ));
    }
    let operator = !args.spectator_only;
    let spectator = !args.operator_only;
    validate_create_web_share_args(&args, operator, spectator)?;
    let expires_at_unix = parse_expires_at(args.expires_at.as_deref())?;
    let scope = resolve_web_share_scope(connection, args.target.as_ref())?;
    if args.kill_session_on_expire && scope.is_pane() {
        return Err(ExitFailure::new(
            1,
            "web-share --kill-session-on-expire requires a session target",
        ));
    }
    let terminal_theme = args.terminal_theme.map(web_terminal_theme);
    let terminal_palette = match terminal_theme {
        Some(WebTerminalTheme::Light | WebTerminalTheme::Dark) => None,
        Some(WebTerminalTheme::User) | None if !std::io::stdout().is_terminal() => None,
        Some(WebTerminalTheme::User) | None => capture_terminal_palette(),
    };
    let controls = operator && scope.is_session();
    Ok(WebShareRequest::Create(CreateWebShareRequest {
        scope,
        public_base_url: args.public_base_url,
        tunnel_provider: args.tunnel_provider,
        frontend_url: args.frontend_url,
        ttl_seconds: args.ttl_seconds,
        expires_at_unix,
        max_spectators: args.max_spectators,
        max_operators: args.max_operators,
        url_options: WebShareUrlOptions {
            no_navbar: args.no_navbar,
            no_disclaimer: args.no_disclaimer,
            show_viewers: !args.hide_viewers,
            terminal_theme,
        },
        require_pin: !args.no_pin,
        operator_pin: args.pin_operator,
        spectator_pin: args.pin_spectator,
        terminal_palette: terminal_palette.map(Box::new),
        operator,
        spectator,
        controls,
        kill_session_on_expire: args.kill_session_on_expire,
    }))
}

fn validate_create_web_share_args(
    args: &WebShareArgs,
    operator: bool,
    spectator: bool,
) -> Result<(), ExitFailure> {
    if args.max_spectators.is_some() && !spectator {
        return Err(ExitFailure::new(
            1,
            "web-share --max-spectators cannot be used with --operator-only",
        ));
    }
    if args.max_operators.is_some() && !operator {
        return Err(ExitFailure::new(
            1,
            "web-share --max-operators cannot be used with --spectator-only",
        ));
    }
    validate_role_pin(args.pin_operator.as_deref(), "--pin-operator")?;
    validate_role_pin(args.pin_spectator.as_deref(), "--pin-spectator")?;
    if args.pin_operator.is_some() && !operator {
        return Err(ExitFailure::new(
            1,
            "web-share --pin-operator cannot be used with --spectator-only",
        ));
    }
    if args.pin_spectator.is_some() && !spectator {
        return Err(ExitFailure::new(
            1,
            "web-share --pin-spectator cannot be used with --operator-only",
        ));
    }
    if args.pin_operator.as_deref() == args.pin_spectator.as_deref() && args.pin_operator.is_some()
    {
        return Err(ExitFailure::new(
            1,
            "web-share operator and spectator PINs must differ",
        ));
    }
    if args.kill_session_on_expire && args.ttl_seconds.is_none() && args.expires_at.is_none() {
        return Err(ExitFailure::new(
            1,
            "web-share --kill-session-on-expire requires --ttl or --expires-at",
        ));
    }
    Ok(())
}

fn validate_role_pin(value: Option<&str>, flag: &str) -> Result<(), ExitFailure> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(());
    }
    Err(ExitFailure::new(
        1,
        format!("web-share {flag} must be exactly 6 ASCII digits"),
    ))
}

fn resolve_web_share_scope(
    connection: &mut rmux_client::Connection,
    target: Option<&TargetSpec>,
) -> Result<WebShareScope, ExitFailure> {
    match target {
        Some(target) => resolve_web_share_target_spec(connection, target),
        None if detect_context() == ClientContext::Outside => {
            create_detached_web_share_session(connection).map(WebShareScope::Session)
        }
        None => Ok(WebShareScope::Pane(PaneTargetRef::slot(
            resolve_current_pane_target(connection, "web-share")?,
        ))),
    }
}

fn create_detached_web_share_session(
    connection: &mut rmux_client::Connection,
) -> Result<SessionName, ExitFailure> {
    let seed = auto_web_share_session_seed();
    for attempt in 0..AUTO_WEB_SHARE_SESSION_ATTEMPTS {
        let session_name = auto_web_share_session_name(seed, attempt)?;
        let response = connection
            .new_session(session_name, true, None)
            .map_err(ExitFailure::from_client)?;
        match response {
            Response::NewSession(created) => return Ok(created.session_name),
            Response::Error(ErrorResponse { error }) if session_already_exists(&error) => {}
            Response::Error(ErrorResponse { error }) => {
                return Err(ExitFailure::new(1, error.to_string()))
            }
            other => return Err(unexpected_response("new-session", &other)),
        }
    }
    Err(ExitFailure::new(
        1,
        "failed to allocate a web-share session name",
    ))
}

fn auto_web_share_session_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default();
    nanos ^ u64::from(std::process::id()).rotate_left(17)
}

fn auto_web_share_session_name(seed: u64, attempt: u32) -> Result<SessionName, ExitFailure> {
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut value = seed.wrapping_add(u64::from(attempt).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    value ^= value >> 33;
    value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
    let mut suffix = String::with_capacity(4);
    for shift in [15, 10, 5, 0] {
        suffix.push(ALPHABET[((value >> shift) & 0x1f) as usize] as char);
    }
    SessionName::new(format!("web-share-{suffix}"))
        .map_err(|error| ExitFailure::new(1, error.to_string()))
}

fn session_already_exists(error: &rmux_proto::RmuxError) -> bool {
    matches!(error, rmux_proto::RmuxError::DuplicateSession(_))
        || error.to_string().contains("already exists")
}

fn resolve_web_share_target_spec(
    connection: &mut rmux_client::Connection,
    target: &TargetSpec,
) -> Result<WebShareScope, ExitFailure> {
    if target_requests_pane_scope(target) {
        let pane = resolve_pane_target_spec(connection, target)?;
        return Ok(WebShareScope::Pane(PaneTargetRef::slot(pane)));
    }
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

fn target_requests_pane_scope(target: &TargetSpec) -> bool {
    matches!(target.exact(), Some(rmux_proto::Target::Pane(_)) | None)
        || raw_target_is_pane_id(target.raw())
}

fn raw_target_is_pane_id(raw: &str) -> bool {
    let Some(pane_id) = raw.strip_prefix('%') else {
        return false;
    };
    !pane_id.is_empty() && pane_id.bytes().all(|byte| byte.is_ascii_digit())
}

const fn web_terminal_theme(value: WebShareTerminalThemeArg) -> WebTerminalTheme {
    match value {
        WebShareTerminalThemeArg::User => WebTerminalTheme::User,
        WebShareTerminalThemeArg::Light => WebTerminalTheme::Light,
        WebShareTerminalThemeArg::Dark => WebTerminalTheme::Dark,
    }
}

fn parse_expires_at(value: Option<&str>) -> Result<Option<u64>, ExitFailure> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|error| {
        ExitFailure::new(
            1,
            format!("web-share --expires-at must be RFC3339: {error}"),
        )
    })?;
    let deadline = SystemTime::from(parsed);
    if deadline <= SystemTime::now() {
        return Err(ExitFailure::new(
            1,
            "web-share --expires-at must be in the future",
        ));
    }
    let unix = deadline
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ExitFailure::new(1, "web-share --expires-at is before the UNIX epoch"))?
        .as_secs();
    Ok(Some(unix))
}

#[cfg(test)]
mod tests {
    use super::{
        auto_web_share_session_name, disconnect_share_output, parse_expires_at,
        target_requests_pane_scope, validate_create_web_share_args,
        validate_web_share_args_without_daemon,
    };
    use crate::cli_args::{parse_target_spec, WebShareArgs};

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

    #[test]
    fn web_share_percent_pane_id_requests_pane_scope() {
        let target = parse_target_spec("%0").expect("percent pane target should parse");
        assert!(matches!(
            target.exact(),
            Some(rmux_proto::Target::Session(_))
        ));
        assert!(target_requests_pane_scope(&target));
    }

    #[test]
    fn web_share_percent_session_name_stays_session_scoped() {
        let target = parse_target_spec("%prod").expect("percent session target should parse");
        assert!(matches!(
            target.exact(),
            Some(rmux_proto::Target::Session(_))
        ));
        assert!(!target_requests_pane_scope(&target));
    }

    #[test]
    fn disconnect_share_output_uses_disconnect_language() {
        let output = disconnect_share_output("abc12345", true);
        assert_eq!(output.stdout(), b"disconnected abc12345\n");
    }

    #[test]
    fn generated_web_share_session_names_use_short_public_prefix() {
        let first = auto_web_share_session_name(1234, 0).expect("name");
        let second = auto_web_share_session_name(1234, 1).expect("name");

        assert!(first.as_str().starts_with("web-share-"));
        assert_eq!(first.as_str().len(), "web-share-".len() + 4);
        assert_ne!(first, second);
    }

    #[test]
    fn missing_tunnel_provider_lists_available_values() {
        let error = validate_web_share_args_without_daemon(&web_share_args_with_provider(""))
            .expect_err("missing provider rejects before daemon connection");

        assert_eq!(
            error.message(),
            "web-share --tunnel-provider requires a provider\nAvailable: localhost-run, sandhole, serveo, srv-us, tailscale-funnel, tailscale-serve."
        );
    }

    #[test]
    fn expires_at_requires_rfc3339() {
        assert!(parse_expires_at(Some("not a date")).is_err());
    }

    #[test]
    fn create_validation_rejects_kill_session_without_deadline_before_scope_resolution() {
        let mut args = web_share_args();
        args.kill_session_on_expire = true;

        let error = validate_create_web_share_args(&args, true, true)
            .expect_err("missing deadline should reject before resolving scope");

        assert_eq!(
            error.message(),
            "web-share --kill-session-on-expire requires --ttl or --expires-at"
        );
    }

    #[test]
    fn create_validation_rejects_role_limits_before_scope_resolution() {
        let mut args = web_share_args();
        args.spectator_only = true;
        args.max_operators = Some(1);

        let error = validate_create_web_share_args(&args, false, true)
            .expect_err("operator limit should reject before resolving scope");

        assert_eq!(
            error.message(),
            "web-share --max-operators cannot be used with --spectator-only"
        );
    }

    fn web_share_args() -> WebShareArgs {
        WebShareArgs {
            list: false,
            stop: None,
            disconnect: None,
            stop_all: false,
            lookup: None,
            config: false,
            target: None,
            operator_only: false,
            spectator_only: false,
            ttl_seconds: None,
            expires_at: None,
            kill_session_on_expire: false,
            max_spectators: None,
            max_operators: None,
            frontend_url: None,
            public_base_url: None,
            tunnel_provider: None,
            no_navbar: false,
            no_disclaimer: false,
            hide_viewers: false,
            show_viewers: false,
            terminal_theme: None,
            no_pin: false,
            pin_operator: None,
            pin_spectator: None,
            pin: false,
        }
    }

    fn web_share_args_with_provider(provider: &str) -> WebShareArgs {
        WebShareArgs {
            tunnel_provider: Some(provider.to_owned()),
            ..web_share_args()
        }
    }
}
