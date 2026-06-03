use std::io;

use ratatui::{
    backend::TestBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use rmux_proto::{CommandOutput, WebShareCreatedResponse};

#[path = "web_share_display/ansi.rs"]
mod ansi;
#[path = "web_share_display/qr.rs"]
mod qr;
#[path = "web_share_display/support.rs"]
mod support;
#[cfg(test)]
#[path = "web_share_display/tests.rs"]
mod tests;

use support::{
    compact_middle, display_url, expiry_label, frontend_label, provider_label, role_limit,
    terminal_width, url_label, LinkMode, UrlLabel,
};

const DEFAULT_WIDTH: u16 = 110;
const MIN_WIDTH: u16 = 44;
const STACK_AT_OR_BELOW: u16 = 80;
const CARD_HEIGHT: u16 = 31;
const OSC8_URL_LABEL_WIDTH: usize = 32;
const ORANGE: Color = Color::Indexed(208);

#[derive(Clone)]
struct ShareCard<'a> {
    title: &'static str,
    subtitle: &'static str,
    color: Color,
    url: &'a str,
    pin: Option<&'a str>,
    limit: Option<String>,
}

pub(super) fn created_share_terminal_output(created: &WebShareCreatedResponse) -> CommandOutput {
    match render_created_share(created) {
        Ok(output) => CommandOutput::from_stdout(output),
        Err(_) => CommandOutput::from_stdout(fallback_output(created)),
    }
}

fn render_created_share(created: &WebShareCreatedResponse) -> io::Result<String> {
    let cards = share_cards(created);
    if cards.is_empty() {
        return Ok(fallback_output(created));
    }

    let width = terminal_width();
    if width < MIN_WIDTH {
        return Ok(too_narrow_output(created, width));
    }

    let link_mode = LinkMode::detect();
    if !cards_fit_width(width, &cards) {
        return Ok(too_narrow_output(created, width));
    }
    let height = render_height(width, &cards, link_mode);
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| {
        let area = frame.area();
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " RMUX web-share ",
                Style::default().fg(Color::Black).bg(Color::LightGreen),
            ));
        frame.render_widget(outer, area);

        let stack = should_stack_cards(area.width.saturating_sub(2), &cards, link_mode);
        let card_rows = if stack {
            CARD_HEIGHT.saturating_mul(cards.len() as u16)
        } else {
            CARD_HEIGHT
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(card_rows),
                Constraint::Length(5),
            ])
            .split(area);

        render_header(frame, chunks[0], created);
        render_cards(frame, chunks[1], &cards, link_mode);
        render_footer(frame, chunks[2], created);
    })?;

    let mut output = ansi::buffer_to_ansi_string(
        terminal.backend().buffer(),
        &cards.iter().map(|card| card.url).collect::<Vec<_>>(),
        link_mode,
    );
    output.push_str(&full_links_output(width, &cards, link_mode));
    Ok(output)
}

fn share_cards(created: &WebShareCreatedResponse) -> Vec<ShareCard<'_>> {
    let mut cards = Vec::new();
    if let Some(url) = created.operator_url.as_deref() {
        cards.push(ShareCard {
            title: "OPERATOR",
            subtitle: "control + type",
            color: Color::LightRed,
            url,
            pin: created.operator_pairing_code.as_deref(),
            limit: created
                .max_operators
                .map(|limit| role_limit(limit, "operator")),
        });
    }
    if let Some(url) = created.spectator_url.as_deref() {
        cards.push(ShareCard {
            title: "SPECTATOR",
            subtitle: "read-only view",
            color: Color::LightBlue,
            url,
            pin: created.spectator_pairing_code.as_deref(),
            limit: created
                .max_spectators
                .map(|limit| role_limit(limit, "spectator")),
        });
    }
    cards
}

fn render_height(width: u16, cards: &[ShareCard<'_>], link_mode: LinkMode) -> u16 {
    let card_rows = if should_stack_cards(width.saturating_sub(2), cards, link_mode) {
        CARD_HEIGHT.saturating_mul(cards.len() as u16)
    } else {
        CARD_HEIGHT
    };
    1 + card_rows + 5 + 2
}

fn should_stack_cards(width: u16, cards: &[ShareCard<'_>], link_mode: LinkMode) -> bool {
    cards.len() > 1 && (width <= STACK_AT_OR_BELOW || width < side_by_side_width(cards, link_mode))
}

fn side_by_side_width(cards: &[ShareCard<'_>], link_mode: LinkMode) -> u16 {
    cards
        .iter()
        .map(|card| card_min_width(card.url, link_mode))
        .sum::<u16>()
        .saturating_add(cards.len().saturating_sub(1) as u16 * 2)
}

fn card_min_width(url: &str, link_mode: LinkMode) -> u16 {
    let qr_width = qr::half_block_qr(url)
        .ok()
        .and_then(|qr| qr.first().map(|line| line.chars().count()))
        .unwrap_or_default();
    let url_width = match link_mode {
        LinkMode::Osc8 => OSC8_URL_LABEL_WIDTH,
        LinkMode::PlainUrl => display_url(url, usize::MAX, link_mode).chars().count(),
    };
    let padding = match link_mode {
        LinkMode::Osc8 => 2,
        LinkMode::PlainUrl => 6,
    };
    qr_width.max(url_width).saturating_add(padding) as u16
}

fn cards_fit_width(width: u16, cards: &[ShareCard<'_>]) -> bool {
    let card_area_width = width.saturating_sub(2);
    cards.iter().all(|card| {
        qr::half_block_qr(card.url)
            .ok()
            .and_then(|qr| qr.first().map(|line| line.chars().count() as u16))
            .is_none_or(|qr_width| card_area_width >= qr_width.saturating_add(2))
    })
}

fn render_header(frame: &mut ratatui::Frame<'_>, area: Rect, created: &WebShareCreatedResponse) {
    let summary = Line::from(vec![
        Span::styled(
            compact_middle(&created.scope.to_string(), 28),
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(provider_label(created), Style::default().fg(Color::Cyan)),
        Span::raw(" · "),
        Span::styled("share ", Style::default().fg(Color::Gray)),
        Span::styled(&created.share_id, Style::default().fg(Color::LightGreen)),
        Span::raw(" · "),
        Span::styled(expiry_label(created), Style::default().fg(ORANGE)),
    ]);
    frame.render_widget(
        Paragraph::new(Text::from(vec![summary])).alignment(Alignment::Center),
        area,
    );
}

fn render_cards(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    cards: &[ShareCard<'_>],
    link_mode: LinkMode,
) {
    let stack = should_stack_cards(area.width, cards, link_mode);
    let constraints = if stack {
        vec![Constraint::Length(CARD_HEIGHT); cards.len()]
    } else {
        vec![Constraint::Percentage(100 / cards.len() as u16); cards.len()]
    };
    let chunks = Layout::default()
        .direction(if stack {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints(constraints)
        .split(area);
    for (card, area) in cards.iter().zip(chunks.iter().copied()) {
        render_card(frame, area, card, link_mode);
    }
}

fn render_card(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    card: &ShareCard<'_>,
    link_mode: LinkMode,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(card.color))
        .title(Span::styled(
            format!(" {} ", card.title),
            Style::default()
                .fg(Color::Black)
                .bg(card.color)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![
        Line::from(Span::styled(
            card.title,
            Style::default().fg(card.color).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            card.subtitle,
            Style::default().fg(Color::Gray),
        )),
    ];
    if let Some(limit) = &card.limit {
        lines.push(Line::from(Span::styled(
            limit.clone(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));

    match qr::half_block_qr(card.url) {
        Ok(qr) => {
            for line in qr {
                lines.push(Line::from(Span::styled(
                    line,
                    Style::default().fg(Color::Black).bg(Color::White),
                )));
            }
        }
        Err(_) => lines.push(Line::from(Span::styled(
            "QR omitted: URL too large",
            Style::default().fg(ORANGE),
        ))),
    }

    lines.push(Line::from(""));
    if let Some(pin) = card.pin {
        lines.push(pin_line(pin));
    }
    let url_width = inner.width.saturating_sub(4) as usize;
    match url_label(card.url, url_width, link_mode) {
        UrlLabel::Clickable(label) => lines.push(Line::from(Span::styled(
            label,
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::UNDERLINED),
        ))),
        UrlLabel::PrintedBelow => lines.push(Line::from(Span::styled(
            "scan QR or copy the full URL below",
            Style::default().fg(Color::Gray),
        ))),
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines)).alignment(Alignment::Center),
        inner,
    );
}

fn full_links_output(width: u16, cards: &[ShareCard<'_>], link_mode: LinkMode) -> String {
    let Some(capacity) = plain_url_capacity(width, cards, link_mode) else {
        return String::new();
    };
    let overflow = cards
        .iter()
        .filter(|card| full_link_needed(card.url, capacity, link_mode))
        .collect::<Vec<_>>();
    if overflow.is_empty() {
        return String::new();
    }

    let mut output = String::from("\nFull web-share URLs:\n");
    for card in overflow {
        output.push_str(&card.title.to_ascii_lowercase());
        output.push_str(": ");
        output.push_str(card.url);
        output.push('\n');
    }
    output
}

fn full_link_needed(url: &str, capacity: usize, link_mode: LinkMode) -> bool {
    match link_mode {
        LinkMode::Osc8 => display_url(url, capacity, link_mode).contains('…'),
        LinkMode::PlainUrl => url.chars().count() > capacity,
    }
}

fn plain_url_capacity(width: u16, cards: &[ShareCard<'_>], link_mode: LinkMode) -> Option<usize> {
    if cards.is_empty() {
        return None;
    }
    let content_width = width.saturating_sub(2);
    let card_width = if should_stack_cards(content_width, cards, link_mode) {
        content_width
    } else {
        content_width / cards.len() as u16
    };
    Some(card_width.saturating_sub(6) as usize)
}

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, created: &WebShareCreatedResponse) {
    let stop_command = format!("rmux web-share stop {}", created.share_id);
    let frontend = frontend_label(created);
    let text = if area.width < 100 {
        Text::from(vec![
            Line::from(vec![
                Span::styled(
                    "encrypted",
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " end-to-end",
                    Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                format!("{frontend} is static; it never receives terminal data"),
                Style::default().fg(Color::Gray),
            )),
            Line::from(vec![
                Span::styled("internet: ", Style::default().fg(Color::Gray)),
                Span::styled("--tunnel-provider NAME", Style::default().fg(Color::Blue)),
            ]),
            Line::from(vec![
                Span::styled("stop: ", Style::default().fg(Color::Gray)),
                Span::styled(stop_command, Style::default().fg(Color::Blue)),
            ]),
        ])
    } else {
        Text::from(vec![
            Line::from(vec![
                Span::styled(frontend.clone(), Style::default().fg(Color::Cyan)),
                Span::styled(" is static · ", Style::default().fg(Color::Gray)),
                Span::styled(
                    "encrypted",
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " end-to-end",
                    Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                format!("{frontend} never receives terminal data"),
                Style::default().fg(Color::Gray),
            )),
            Line::from(vec![
                Span::styled("internet: ", Style::default().fg(Color::Gray)),
                Span::styled("--tunnel-provider NAME", Style::default().fg(Color::Blue)),
                Span::raw(" · "),
                Span::styled("frontend: ", Style::default().fg(Color::Gray)),
                Span::styled("--frontend-url URL", Style::default().fg(Color::Blue)),
            ]),
            Line::from(vec![
                Span::styled("stop: ", Style::default().fg(Color::Gray)),
                Span::styled(stop_command, Style::default().fg(Color::Blue)),
            ]),
        ])
    };
    frame.render_widget(Paragraph::new(text).alignment(Alignment::Left), area);
}

fn pin_line(code: &str) -> Line<'static> {
    let grouped = if code.len() == 6 {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    };
    Line::from(vec![
        Span::styled(
            " PIN ",
            Style::default()
                .fg(Color::Black)
                .bg(ORANGE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            grouped,
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
        ),
    ])
}

pub(super) fn ansi_fg(color: Color) -> &'static str {
    match color {
        Color::Black => "\x1b[30m",
        Color::White => "\x1b[37m",
        Color::Blue => "\x1b[34m",
        Color::Gray | Color::DarkGray => "\x1b[90m",
        Color::Cyan => "\x1b[36m",
        Color::LightRed => "\x1b[91m",
        Color::LightBlue => "\x1b[94m",
        Color::LightGreen => "\x1b[92m",
        Color::Indexed(208) => "\x1b[38;5;208m",
        _ => "\x1b[39m",
    }
}

pub(super) fn ansi_bg(color: Color) -> &'static str {
    match color {
        Color::Black => "\x1b[40m",
        Color::White => "\x1b[47m",
        Color::LightRed => "\x1b[101m",
        Color::LightBlue => "\x1b[104m",
        Color::LightGreen => "\x1b[102m",
        Color::Indexed(208) => "\x1b[48;5;208m",
        _ => "\x1b[49m",
    }
}

fn fallback_output(created: &WebShareCreatedResponse) -> String {
    let mut output = String::new();
    if let Some(url) = &created.operator_url {
        output.push_str("operator ");
        output.push_str(url);
        output.push('\n');
    }
    if let Some(url) = &created.spectator_url {
        output.push_str("spectator ");
        output.push_str(url);
        output.push('\n');
    }
    if let Some(pin) = &created.operator_pairing_code {
        output.push_str("operator pin ");
        output.push_str(pin);
        output.push('\n');
    }
    if let Some(pin) = &created.spectator_pairing_code {
        output.push_str("spectator pin ");
        output.push_str(pin);
        output.push('\n');
    }
    output
}

fn too_narrow_output(created: &WebShareCreatedResponse, width: u16) -> String {
    let mut output = format!("RMUX web-share\n\nterminal too narrow ({width} cols)\n\n");
    output.push_str(&fallback_output(created));
    output
}
