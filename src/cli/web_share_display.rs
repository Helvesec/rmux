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

mod ansi;
mod plain;
mod qr;
mod support;
#[cfg(test)]
#[path = "web_share_display/tests.rs"]
mod tests;

use support::{
    compact_middle, display_url, expiry_label, frontend_label, provider_label, role_limit,
    terminal_needs_qr_fallback, terminal_width, url_label, LinkMode, OutputStyle, UrlLabel,
};

const DEFAULT_WIDTH: u16 = 110;
const MIN_WIDTH: u16 = 44;
const STACK_AT_OR_BELOW: u16 = 80;
const OSC8_URL_LABEL_WIDTH: usize = 32;
const PRINTED_BELOW_LABEL: &str = "scan QR or copy the full URL below";
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
    render_created_share_with_style(created, OutputStyle::detect())
}

fn render_created_share_with_style(
    created: &WebShareCreatedResponse,
    style: OutputStyle,
) -> io::Result<String> {
    let cards = share_cards(created);
    if cards.is_empty() {
        return Ok(fallback_output(created));
    }

    let width = terminal_width();
    if width < MIN_WIDTH {
        return Ok(too_narrow_output(created, width));
    }

    let link_mode = link_mode_for_style(style);
    let qr_mode = qr_render_mode(style);
    if !cards_fit_width(width, &cards, qr_mode) {
        return Ok(too_narrow_output(created, width));
    }
    let height = render_height(width, &cards, link_mode, qr_mode);
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

        let card_rows = cards_height(area.width.saturating_sub(2), &cards, link_mode, qr_mode);
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
        render_cards(frame, chunks[1], &cards, link_mode, qr_mode);
        render_footer(frame, chunks[2], created);
    })?;

    let links = cards.iter().map(|card| card.url).collect::<Vec<_>>();
    let mut output = match style {
        OutputStyle::Ansi => {
            ansi::buffer_to_ansi_string(terminal.backend().buffer(), &links, link_mode)
        }
        OutputStyle::Plain => plain::buffer_to_plain_string(terminal.backend().buffer()),
    };
    output.push_str(&full_links_output_for_style(
        width, &cards, link_mode, style,
    ));
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

fn render_height(
    width: u16,
    cards: &[ShareCard<'_>],
    link_mode: LinkMode,
    qr_mode: qr::RenderMode,
) -> u16 {
    let card_rows = cards_height(width.saturating_sub(2), cards, link_mode, qr_mode);
    1 + card_rows + 5 + 2
}

fn should_stack_cards(width: u16, cards: &[ShareCard<'_>], link_mode: LinkMode) -> bool {
    should_stack_cards_with_qr(width, cards, link_mode, qr::RenderMode::Compact)
}

fn should_stack_cards_with_qr(
    width: u16,
    cards: &[ShareCard<'_>],
    link_mode: LinkMode,
    qr_mode: qr::RenderMode,
) -> bool {
    cards.len() > 1
        && (width <= STACK_AT_OR_BELOW || width < side_by_side_width(cards, link_mode, qr_mode))
}

fn side_by_side_width(
    cards: &[ShareCard<'_>],
    link_mode: LinkMode,
    qr_mode: qr::RenderMode,
) -> u16 {
    cards
        .iter()
        .map(|card| card_min_width(card.url, link_mode, qr_mode))
        .sum::<u16>()
        .saturating_add(cards.len().saturating_sub(1) as u16 * 2)
}

fn card_min_width(url: &str, link_mode: LinkMode, qr_mode: qr::RenderMode) -> u16 {
    let qr_width = qr_width(url, qr_mode);
    let url_width = match link_mode {
        LinkMode::Osc8 => OSC8_URL_LABEL_WIDTH,
        LinkMode::PlainUrl => display_url(url, usize::MAX, link_mode)
            .chars()
            .count()
            .min(PRINTED_BELOW_LABEL.chars().count()),
    };
    let padding = match link_mode {
        LinkMode::Osc8 => 2,
        LinkMode::PlainUrl => 6,
    };
    qr_width.max(url_width).saturating_add(padding) as u16
}

fn cards_fit_width(width: u16, cards: &[ShareCard<'_>], qr_mode: qr::RenderMode) -> bool {
    let card_area_width = width.saturating_sub(2);
    cards.iter().all(|card| {
        u16::try_from(qr_width(card.url, qr_mode))
            .ok()
            .is_none_or(|qr_width| card_area_width >= qr_width.saturating_add(2))
    })
}

fn cards_height(
    width: u16,
    cards: &[ShareCard<'_>],
    link_mode: LinkMode,
    qr_mode: qr::RenderMode,
) -> u16 {
    if should_stack_cards_with_qr(width, cards, link_mode, qr_mode) {
        cards.iter().map(|card| card_height(card, qr_mode)).sum()
    } else {
        cards
            .iter()
            .map(|card| card_height(card, qr_mode))
            .max()
            .unwrap_or(0)
    }
}

fn card_height(card: &ShareCard<'_>, qr_mode: qr::RenderMode) -> u16 {
    card_line_count(card, qr_mode).saturating_add(2)
}

fn card_line_count(card: &ShareCard<'_>, qr_mode: qr::RenderMode) -> u16 {
    let header_lines = 3 + u16::from(card.limit.is_some());
    let qr_lines = u16::try_from(qr_height(card.url, qr_mode)).unwrap_or(1);
    let tail_lines = 2 + u16::from(card.pin.is_some());
    header_lines
        .saturating_add(qr_lines)
        .saturating_add(tail_lines)
}

fn qr_width(url: &str, qr_mode: qr::RenderMode) -> usize {
    qr::width(url, qr_mode)
}

fn qr_height(url: &str, qr_mode: qr::RenderMode) -> usize {
    qr::height(url, qr_mode)
}

fn link_mode_for_style(style: OutputStyle) -> LinkMode {
    match style {
        OutputStyle::Ansi => LinkMode::detect(),
        OutputStyle::Plain => LinkMode::PlainUrl,
    }
}

fn qr_render_mode(style: OutputStyle) -> qr::RenderMode {
    if matches!(style, OutputStyle::Plain) {
        qr::RenderMode::Plain
    } else if terminal_needs_qr_fallback() {
        qr::RenderMode::TerminalSafe
    } else {
        qr::RenderMode::Compact
    }
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
    qr_mode: qr::RenderMode,
) {
    let stack = should_stack_cards_with_qr(area.width, cards, link_mode, qr_mode);
    let constraints = if stack {
        cards
            .iter()
            .map(|card| Constraint::Length(card_height(card, qr_mode)))
            .collect()
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
        render_card(frame, area, card, link_mode, qr_mode);
    }
}

fn render_card(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    card: &ShareCard<'_>,
    link_mode: LinkMode,
    qr_mode: qr::RenderMode,
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

    match qr::render_lines(card.url, qr_mode) {
        Ok(qr_lines) => lines.extend(qr_lines),
        Err(_) => lines.push(qr_omitted_line()),
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
            PRINTED_BELOW_LABEL,
            Style::default().fg(Color::Gray),
        ))),
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines)).alignment(Alignment::Center),
        inner,
    );
}

fn qr_omitted_line() -> Line<'static> {
    Line::from(Span::styled(
        "QR omitted: URL too large",
        Style::default().fg(ORANGE),
    ))
}

#[cfg(test)]
fn full_links_output(width: u16, cards: &[ShareCard<'_>], link_mode: LinkMode) -> String {
    full_links_output_for_style(width, cards, link_mode, OutputStyle::detect())
}

fn full_links_output_for_style(
    width: u16,
    cards: &[ShareCard<'_>],
    link_mode: LinkMode,
    style: OutputStyle,
) -> String {
    let Some(capacity) = plain_url_capacity(width, cards, link_mode) else {
        return String::new();
    };
    let include_all = terminal_needs_qr_fallback() || matches!(style, OutputStyle::Plain);
    full_links_output_with_copy_fallback(capacity, cards, link_mode, include_all)
}

fn full_links_output_with_copy_fallback(
    capacity: usize,
    cards: &[ShareCard<'_>],
    link_mode: LinkMode,
    include_all: bool,
) -> String {
    let overflow = cards
        .iter()
        .filter(|card| include_all || full_link_needed(card.url, capacity, link_mode))
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
    match url_label(url, capacity, link_mode) {
        UrlLabel::Clickable(label) => label != url,
        UrlLabel::PrintedBelow => true,
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
            code.to_owned(),
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
        Color::Indexed(15) => "\x1b[38;5;15m",
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
        Color::Indexed(15) => "\x1b[48;5;15m",
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
