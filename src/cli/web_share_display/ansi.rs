use ratatui::{
    buffer::{Buffer, Cell},
    style::{Color, Modifier},
};

use super::support::{compact_url, LinkMode};
use super::{ansi_bg, ansi_fg, OSC8_URL_LABEL_WIDTH};

pub(super) fn buffer_to_ansi_string(
    buffer: &Buffer,
    links: &[&str],
    link_mode: LinkMode,
) -> String {
    let mut output = String::new();
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        let last = (area.left()..area.right())
            .rev()
            .find(|x| cell_is_meaningful(&buffer[(*x, y)]));
        let Some(last) = last else {
            output.push('\n');
            continue;
        };

        let line = visible_line(buffer, y);
        let hyperlinks = hyperlinks_for_line(&line, links, link_mode);
        let mut active_link: Option<&Hyperlink<'_>> = None;
        for x in area.left()..=last {
            let column = x.saturating_sub(area.left());
            if active_link.is_some_and(|link| column >= link.end) {
                close_hyperlink(&mut output, link_mode);
                active_link = None;
            }
            if active_link.is_none() {
                active_link = hyperlinks.iter().find(|link| column == link.start);
            }
            if let Some(link) = active_link.filter(|link| column == link.start) {
                open_hyperlink(&mut output, link.href, link_mode);
            }

            let cell = &buffer[(x, y)];
            if active_link.is_some() {
                output.push_str(cell.symbol());
            } else {
                write_styled_cell(&mut output, cell);
            }
        }
        if active_link.is_some() {
            close_hyperlink(&mut output, link_mode);
        }
        output.push('\n');
    }
    output
}

fn open_hyperlink(output: &mut String, href: &str, link_mode: LinkMode) {
    if link_mode.supports_osc8() {
        output.push_str(&format!("\x1b]8;;{href}\x1b\\"));
        output.push_str(ansi_fg(Color::Blue));
        output.push_str("\x1b[49m\x1b[4m");
    }
}

fn close_hyperlink(output: &mut String, link_mode: LinkMode) {
    if link_mode.supports_osc8() {
        output.push_str("\x1b[0m\x1b]8;;\x1b\\");
    }
}

struct Hyperlink<'a> {
    start: u16,
    end: u16,
    href: &'a str,
}

fn visible_line(buffer: &Buffer, y: u16) -> String {
    let area = buffer.area;
    let mut line = String::new();
    for x in area.left()..area.right() {
        line.push_str(buffer[(x, y)].symbol());
    }
    line
}

fn hyperlinks_for_line<'a>(
    line: &str,
    links: &'a [&'a str],
    link_mode: LinkMode,
) -> Vec<Hyperlink<'a>> {
    match link_mode {
        LinkMode::Osc8 => osc8_hyperlinks_for_line(line, links),
        LinkMode::PlainUrl => plain_url_segments_for_line(line, links),
    }
}

fn osc8_hyperlinks_for_line<'a>(line: &str, links: &'a [&'a str]) -> Vec<Hyperlink<'a>> {
    let mut hyperlinks = Vec::new();
    let mut occupied = Vec::new();
    for (label, href) in unique_osc8_labels_for_line(line, links) {
        let mut search_from = 0usize;
        while let Some(offset) = line[search_from..].find(&label) {
            let start_byte = search_from + offset;
            let start = line[..start_byte].chars().count();
            let length = label.chars().count();
            let end = start + length;
            if !occupied
                .iter()
                .any(|(known_start, known_end)| start < *known_end && end > *known_start)
            {
                hyperlinks.push(Hyperlink {
                    start: start as u16,
                    end: end as u16,
                    href,
                });
                occupied.push((start, end));
            }
            search_from = start_byte + label.len();
        }
    }
    hyperlinks.sort_by_key(|link| link.start);
    hyperlinks
}

fn unique_osc8_labels_for_line<'a>(line: &str, links: &'a [&'a str]) -> Vec<(String, &'a str)> {
    let mut labels: Vec<(String, Option<&'a str>)> = Vec::new();
    for href in links.iter().copied() {
        for label in osc8_label_candidates(href) {
            if !line.contains(&label) {
                continue;
            }
            if let Some((_, existing)) = labels.iter_mut().find(|(known, _)| *known == label) {
                if *existing != Some(href) {
                    *existing = None;
                }
                continue;
            }
            labels.push((label, Some(href)));
        }
    }
    let mut labels = labels
        .into_iter()
        .filter_map(|(label, href)| href.map(|href| (label, href)))
        .collect::<Vec<_>>();
    labels.sort_by_key(|(label, _)| std::cmp::Reverse(label.chars().count()));
    labels
}

fn osc8_label_candidates(href: &str) -> Vec<String> {
    let mut labels = Vec::new();
    for width in (1..=OSC8_URL_LABEL_WIDTH).rev() {
        let label = compact_url(href, width);
        if label.is_empty() || labels.iter().any(|known| known == &label) {
            continue;
        }
        labels.push(label);
    }
    labels
}

fn plain_url_segments_for_line<'a>(line: &str, links: &'a [&'a str]) -> Vec<Hyperlink<'a>> {
    let mut hyperlinks = Vec::new();
    for href in links.iter().copied() {
        let mut search_from = 0usize;
        while let Some(offset) = line[search_from..].find(href) {
            let start_byte = search_from + offset;
            let start = line[..start_byte].chars().count() as u16;
            let length = href.chars().count() as u16;
            hyperlinks.push(Hyperlink {
                start,
                end: start.saturating_add(length),
                href,
            });
            search_from = start_byte + href.len();
        }
    }
    hyperlinks.sort_by_key(|link| link.start);
    hyperlinks
}

fn write_styled_cell(output: &mut String, cell: &Cell) {
    output.push_str(ansi_fg(cell.fg));
    output.push_str(ansi_bg(cell.bg));
    if cell.modifier.contains(Modifier::BOLD) {
        output.push_str("\x1b[1m");
    }
    if cell.modifier.contains(Modifier::UNDERLINED) {
        output.push_str("\x1b[4m");
    }
    output.push_str(cell.symbol());
    output.push_str("\x1b[0m");
}

fn cell_is_meaningful(cell: &Cell) -> bool {
    cell.symbol() != " "
        || cell.fg != Color::Reset
        || cell.bg != Color::Reset
        || !cell.modifier.is_empty()
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect};

    use super::super::support::{compact_url, LinkMode};
    use super::super::OSC8_URL_LABEL_WIDTH;
    use super::buffer_to_ansi_string;

    #[test]
    fn osc8_links_tunneled_share_url_with_endpoint_fragment() {
        let href = "https://share.rmux.io/#e=wss%3A%2F%2Ftunnel.example%2Fws&t=abcdefghijklmnopqrstuvwxyz0123456789";
        let label = compact_url(href, OSC8_URL_LABEL_WIDTH);
        let buffer = buffer_with_text(&label);

        let output = buffer_to_ansi_string(&buffer, &[href], LinkMode::Osc8);

        assert!(output.contains(&format!("\x1b]8;;{href}\x1b\\")));
        assert!(output.contains(&label));
        assert!(output.contains("\x1b]8;;\x1b\\"));
    }

    #[test]
    fn osc8_links_share_url_compacted_to_rendered_card_width() {
        let href = "https://share.rmux.io/#e=wss%3A%2F%2Ftail.example%2Fws&t=abcdefghijklmnopqrstuvwxyz0123456789";
        let label = compact_url(href, 28);
        assert_ne!(label, compact_url(href, OSC8_URL_LABEL_WIDTH));
        let buffer = buffer_with_text(&label);

        let output = buffer_to_ansi_string(&buffer, &[href], LinkMode::Osc8);

        assert!(output.contains(&format!("\x1b]8;;{href}\x1b\\")));
        assert!(output.contains(&label));
    }

    #[test]
    fn osc8_skips_ambiguous_compact_labels() {
        let first =
            "https://share.rmux.io/#e=wss%3A%2F%2Fone.example%2Fws&t=abcdefghijklmnopqrstuvwxyz";
        let second =
            "https://share.rmux.io/#e=wss%3A%2F%2Ftwo.example%2Fws&t=abcdefghijklmnopqrstuvwxyz";
        let label = compact_url(first, OSC8_URL_LABEL_WIDTH);
        assert_eq!(label, compact_url(second, OSC8_URL_LABEL_WIDTH));

        let buffer = buffer_with_text(&label);
        let output = buffer_to_ansi_string(&buffer, &[first, second], LinkMode::Osc8);

        assert!(!output.contains("\x1b]8;;https://share.rmux.io/"));
    }

    fn buffer_with_text(text: &str) -> Buffer {
        let width = text.chars().count() as u16;
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
        for (x, ch) in text.chars().enumerate() {
            buffer[(x as u16, 0)].set_symbol(&ch.to_string());
        }
        buffer
    }
}
