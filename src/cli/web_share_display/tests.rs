use ratatui::style::Color;
use rmux_proto::{CommandOutput, WebShareCreatedResponse, WebShareScope};

use super::support::{url_label, LinkMode, UrlLabel};
use super::{cards_fit_width, created_share_terminal_output, full_links_output, ShareCard};

#[test]
fn created_output_includes_distinct_role_pins() {
    let created = WebShareCreatedResponse {
        share_id: "abc12345".to_owned(),
        scope: WebShareScope::Session("demo".parse().expect("session")),
        spectator_url: Some("https://share.rmux.io/#t=spectator".to_owned()),
        operator_url: Some("https://share.rmux.io/#t=operator".to_owned()),
        tunnel_provider: Some("NAME".to_owned()),
        tunnel_public_url: None,
        expires_at_unix: None,
        operator_pairing_code: Some("123456".to_owned()),
        spectator_pairing_code: Some("654321".to_owned()),
        max_spectators: Some(12),
        max_operators: Some(1),
        operator: true,
        spectator: true,
        controls: true,
        kill_session_on_expire: false,
        output: CommandOutput::from_stdout(Vec::new()),
    };

    let rendered = String::from_utf8(created_share_terminal_output(&created).stdout().to_vec())
        .expect("utf8 output");
    let visible = strip_ansi(&rendered);

    assert!(visible.contains("123 456"));
    assert!(visible.contains("654 321"));
    assert!(visible.contains("share.rmux.io is static"));
}

#[test]
fn plain_terminal_urls_that_do_not_fit_are_printed_below() {
    let url =
        "https://share.rmux.io/#t=abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz";
    let card = ShareCard {
        title: "SPECTATOR",
        subtitle: "read-only view",
        color: Color::LightBlue,
        url,
        pin: Some("654321"),
        limit: None,
    };

    assert_eq!(
        url_label(url, 40, LinkMode::PlainUrl),
        UrlLabel::PrintedBelow
    );
    let links = full_links_output(52, &[card], LinkMode::PlainUrl);

    assert!(links.contains("Full web-share URLs:"));
    assert!(links.contains(url));
    assert!(!links.contains('…'));
}

#[test]
fn osc8_compact_urls_are_printed_below_as_copy_fallback() {
    let url = "https://share.rmux.io/#e=wss%3A%2F%2Ftail.example%2Fws&t=abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz";
    let card = ShareCard {
        title: "SPECTATOR",
        subtitle: "read-only view",
        color: Color::LightBlue,
        url,
        pin: None,
        limit: None,
    };

    let links = full_links_output(52, &[card], LinkMode::Osc8);

    assert!(links.contains("Full web-share URLs:"));
    assert!(links.contains(url));
    assert!(!links.contains('…'));
}

#[test]
fn narrow_single_card_rejects_truncated_qr_width() {
    let url = "https://share.rmux.io/#e=wss%3A%2F%2Ftunnel.example%2Fws&t=abcdefghijklmnopqrstuvwxyz0123456789";
    let card = ShareCard {
        title: "SPECTATOR",
        subtitle: "read-only view",
        color: Color::LightBlue,
        url,
        pin: None,
        limit: None,
    };

    assert!(!cards_fit_width(44, std::slice::from_ref(&card)));
    assert!(cards_fit_width(46, &[card]));
}

#[test]
fn created_output_uses_custom_frontend_host() {
    let created = WebShareCreatedResponse {
        share_id: "abc12345".to_owned(),
        scope: WebShareScope::Session("demo".parse().expect("session")),
        spectator_url: Some("https://share.fork.example/share/#t=spectator".to_owned()),
        operator_url: None,
        tunnel_provider: None,
        tunnel_public_url: None,
        expires_at_unix: None,
        operator_pairing_code: None,
        spectator_pairing_code: Some("654321".to_owned()),
        max_spectators: None,
        max_operators: None,
        operator: false,
        spectator: true,
        controls: false,
        kill_session_on_expire: false,
        output: CommandOutput::from_stdout(Vec::new()),
    };

    let rendered = String::from_utf8(created_share_terminal_output(&created).stdout().to_vec())
        .expect("utf8 output");
    let visible = strip_ansi(&rendered);

    assert!(visible.contains("share.fork.example is static"));
    assert!(!visible.contains("share.rmux.io is static"));
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            output.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some(']') => {
                chars.next();
                while let Some(ch) = chars.next() {
                    if ch == '\x07' {
                        break;
                    }
                    if ch == '\x1b' && chars.next_if_eq(&'\\').is_some() {
                        break;
                    }
                }
            }
            Some('[') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    output
}
