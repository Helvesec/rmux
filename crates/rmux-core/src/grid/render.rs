use crate::hyperlinks::Hyperlinks;
use crate::input::{Colour, GridAttr, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB};

use super::{GridCell, GridCellFlags};

pub(super) fn append_cell_text(cell: &GridCell, output: &mut String, escape_sequences: bool) {
    if cell.flags.contains(GridCellFlags::TAB) {
        output.push('\t');
        return;
    }

    if !escape_sequences {
        output.push_str(cell.text());
        return;
    }

    let text = cell.text();
    if text.len() == 1 {
        let byte = text.as_bytes()[0];
        if byte == b'\\' {
            output.push_str("\\\\");
        } else if is_printable_capture_byte(byte) {
            output.push(char::from(byte));
        } else {
            push_octal_escape(output, byte);
        }
        return;
    }

    output.push_str(text);
}

fn is_printable_capture_byte(byte: u8) -> bool {
    byte >= b' ' && byte != b'\\' && byte != 0x7f
}

pub(super) fn append_grid_string_code(
    lastgc: &GridCell,
    gc: &GridCell,
    output: &mut String,
    escape_sequences: bool,
    hyperlinks: Option<&Hyperlinks>,
    has_link: &mut bool,
) {
    let attr = gc.attr();
    let mut lastattr = lastgc.attr();
    let mut sgr = Vec::new();

    for &(mask, _code) in ATTR_CODES {
        if ((attr & mask) == 0 && (lastattr & mask) != 0)
            || (lastgc.us() != COLOUR_DEFAULT && gc.us() == COLOUR_DEFAULT)
        {
            sgr.push(0);
            lastattr &= GridAttr::CHARSET;
            break;
        }
    }
    for &(mask, code) in ATTR_CODES {
        if (attr & mask) != 0 && (lastattr & mask) == 0 {
            sgr.push(code);
        }
    }

    if !sgr.is_empty() {
        append_escape_prefix(output, escape_sequences, '[');
        for (index, code) in sgr.iter().enumerate() {
            if index > 0 {
                output.push(';');
            }
            push_attr_code(output, *code);
        }
        output.push('m');
    }

    append_colour_code(
        output,
        &colour_codes_fg(gc.fg()),
        &colour_codes_fg(lastgc.fg()),
        !sgr.is_empty() && sgr[0] == 0,
        escape_sequences,
    );
    append_colour_code(
        output,
        &colour_codes_bg(gc.bg()),
        &colour_codes_bg(lastgc.bg()),
        !sgr.is_empty() && sgr[0] == 0,
        escape_sequences,
    );
    append_colour_code(
        output,
        &colour_codes_us(gc.us()),
        &colour_codes_us(lastgc.us()),
        !sgr.is_empty() && sgr[0] == 0,
        escape_sequences,
    );

    if (attr & GridAttr::CHARSET) != 0 && (lastattr & GridAttr::CHARSET) == 0 {
        output.push_str(if escape_sequences {
            "\\016"
        } else {
            "\u{000e}"
        });
    }
    if (attr & GridAttr::CHARSET) == 0 && (lastattr & GridAttr::CHARSET) != 0 {
        output.push_str(if escape_sequences {
            "\\017"
        } else {
            "\u{000f}"
        });
    }

    if let Some(hyperlinks) = hyperlinks
        && lastgc.link() != gc.link() {
            if let Some(link) = hyperlinks.get(gc.link()) {
                append_hyperlink(output, &link.internal_id, &link.uri, escape_sequences);
                *has_link = true;
            } else if *has_link {
                append_hyperlink(output, "", "", escape_sequences);
                *has_link = false;
            }
        }
}

fn append_colour_code(
    output: &mut String,
    newc: &[i32],
    oldc: &[i32],
    reset: bool,
    escape_sequences: bool,
) {
    if newc.is_empty() {
        return;
    }
    if !reset && newc == oldc {
        return;
    }
    if reset && matches!(newc.first(), Some(39 | 49)) {
        return;
    }

    append_escape_prefix(output, escape_sequences, '[');
    for (index, value) in newc.iter().enumerate() {
        if index > 0 {
            output.push(';');
        }
        output.push_str(&value.to_string());
    }
    output.push('m');
}

fn colour_codes_fg(colour: Colour) -> Vec<i32> {
    if colour & COLOUR_FLAG_256 != 0 {
        return vec![38, 5, colour & 0xff];
    }
    if colour & COLOUR_FLAG_RGB != 0 {
        let (r, g, b) = split_rgb(colour);
        return vec![38, 2, i32::from(r), i32::from(g), i32::from(b)];
    }

    match colour {
        0..=7 => vec![colour + 30],
        COLOUR_DEFAULT => vec![39],
        90..=97 => vec![colour],
        _ => Vec::new(),
    }
}

fn colour_codes_bg(colour: Colour) -> Vec<i32> {
    if colour & COLOUR_FLAG_256 != 0 {
        return vec![48, 5, colour & 0xff];
    }
    if colour & COLOUR_FLAG_RGB != 0 {
        let (r, g, b) = split_rgb(colour);
        return vec![48, 2, i32::from(r), i32::from(g), i32::from(b)];
    }

    match colour {
        0..=7 => vec![colour + 40],
        COLOUR_DEFAULT => vec![49],
        90..=97 => vec![colour + 10],
        _ => Vec::new(),
    }
}

fn colour_codes_us(colour: Colour) -> Vec<i32> {
    if colour & COLOUR_FLAG_256 != 0 {
        return vec![58, 5, colour & 0xff];
    }
    if colour & COLOUR_FLAG_RGB != 0 {
        let (r, g, b) = split_rgb(colour);
        return vec![58, 2, i32::from(r), i32::from(g), i32::from(b)];
    }
    Vec::new()
}

fn split_rgb(colour: Colour) -> (u8, u8, u8) {
    (
        ((colour >> 16) & 0xff) as u8,
        ((colour >> 8) & 0xff) as u8,
        (colour & 0xff) as u8,
    )
}

fn push_attr_code(output: &mut String, code: i32) {
    if code < 10 {
        output.push_str(&code.to_string());
    } else {
        output.push_str(&(code / 10).to_string());
        output.push(':');
        output.push_str(&(code % 10).to_string());
    }
}

fn append_escape_prefix(output: &mut String, escape_sequences: bool, suffix: char) {
    if escape_sequences {
        output.push_str("\\033");
    } else {
        output.push('\u{001b}');
    }
    output.push(suffix);
}

pub(super) fn append_hyperlink(output: &mut String, id: &str, uri: &str, escape_sequences: bool) {
    append_escape_prefix(output, escape_sequences, ']');
    output.push('8');
    output.push(';');
    if id.is_empty() {
        output.push(';');
    } else {
        output.push_str("id=");
        output.push_str(id);
        output.push(';');
    }
    output.push_str(uri);
    if escape_sequences {
        output.push_str("\\033\\\\");
    } else {
        output.push('\u{001b}');
        output.push('\\');
    }
}

fn push_octal_escape(output: &mut String, byte: u8) {
    output.push('\\');
    output.push(char::from(b'0' + ((byte >> 6) & 0x7)));
    output.push(char::from(b'0' + ((byte >> 3) & 0x7)));
    output.push(char::from(b'0' + (byte & 0x7)));
}

const ATTR_CODES: &[(u16, i32)] = &[
    (GridAttr::BRIGHT, 1),
    (GridAttr::DIM, 2),
    (GridAttr::ITALICS, 3),
    (GridAttr::UNDERSCORE, 4),
    (GridAttr::BLINK, 5),
    (GridAttr::REVERSE, 7),
    (GridAttr::HIDDEN, 8),
    (GridAttr::STRIKETHROUGH, 9),
    (GridAttr::UNDERSCORE_2, 42),
    (GridAttr::UNDERSCORE_3, 43),
    (GridAttr::UNDERSCORE_4, 44),
    (GridAttr::UNDERSCORE_5, 45),
    (GridAttr::OVERLINE, 53),
];
