//! SGR (Select Graphic Rendition) dispatch matching tmux `input_csi_dispatch_sgr`.

use super::cell::{GridAttr, GridCell};
use super::colour::{colour_join_rgb, COLOUR_DEFAULT, COLOUR_FLAG_256};
use super::params::ParamType;
use super::InputParser;

/// Handle CSI SGR sequence.
pub(crate) fn dispatch_sgr(parser: &mut InputParser) {
    if parser.param_list.len() == 0 {
        // SGR with no params: reset to default.
        let link = parser.cell.cell.link;
        parser.cell.cell = GridCell::default();
        parser.cell.cell.link = link;
        return;
    }

    let mut i: u32 = 0;
    while i < parser.param_list.len() {
        // Check for colon-separated ISO form (InputParam::Str).
        if let Some(param) = parser.param_list.param_at(i)
            && let ParamType::Str(s) = &param.ptype {
                let s = s.clone();
                dispatch_sgr_colon(&mut parser.cell.cell, &s);
                i += 1;
                continue;
            }

        let n = parser.param_list.get(i, 0, 0);
        if n == -1 {
            i += 1;
            continue;
        }

        if n == 38 || n == 48 || n == 58 {
            i += 1;
            match parser.param_list.get(i, 0, -1) {
                2 => {
                    // RGB: read r, g, b from params.
                    let r = parser.param_list.get(i + 1, 0, -1);
                    let g = parser.param_list.get(i + 2, 0, -1);
                    let b = parser.param_list.get(i + 3, 0, -1);
                    if r != -1 && r <= 255 && g != -1 && g <= 255 && b != -1 && b <= 255 {
                        let c = colour_join_rgb(r as u8, g as u8, b as u8);
                        match n {
                            38 => parser.cell.cell.fg = c,
                            48 => parser.cell.cell.bg = c,
                            58 => parser.cell.cell.us = c,
                            _ => {}
                        }
                        i += 3;
                    }
                }
                5 => {
                    // 256-colour.
                    let c = parser.param_list.get(i + 1, 0, -1);
                    if c == -1 || c > 255 {
                        match n {
                            38 => parser.cell.cell.fg = COLOUR_DEFAULT,
                            48 => parser.cell.cell.bg = COLOUR_DEFAULT,
                            _ => {}
                        }
                    } else {
                        match n {
                            38 => parser.cell.cell.fg = c | COLOUR_FLAG_256,
                            48 => parser.cell.cell.bg = c | COLOUR_FLAG_256,
                            58 => parser.cell.cell.us = c | COLOUR_FLAG_256,
                            _ => {}
                        }
                    }
                    i += 1;
                }
                _ => {}
            }
            i += 1;
            continue;
        }

        let gc = &mut parser.cell.cell;
        match n {
            0 => {
                let link = gc.link;
                *gc = GridCell::default();
                gc.link = link;
            }
            1 => gc.attr |= GridAttr::BRIGHT,
            2 => gc.attr |= GridAttr::DIM,
            3 => gc.attr |= GridAttr::ITALICS,
            4 => {
                gc.attr &= !GridAttr::ALL_UNDERSCORE;
                gc.attr |= GridAttr::UNDERSCORE;
            }
            5 | 6 => gc.attr |= GridAttr::BLINK,
            7 => gc.attr |= GridAttr::REVERSE,
            8 => gc.attr |= GridAttr::HIDDEN,
            9 => gc.attr |= GridAttr::STRIKETHROUGH,
            21 => {
                gc.attr &= !GridAttr::ALL_UNDERSCORE;
                gc.attr |= GridAttr::UNDERSCORE_2;
            }
            22 => gc.attr &= !(GridAttr::BRIGHT | GridAttr::DIM),
            23 => gc.attr &= !GridAttr::ITALICS,
            24 => gc.attr &= !GridAttr::ALL_UNDERSCORE,
            25 => gc.attr &= !GridAttr::BLINK,
            27 => gc.attr &= !GridAttr::REVERSE,
            28 => gc.attr &= !GridAttr::HIDDEN,
            29 => gc.attr &= !GridAttr::STRIKETHROUGH,
            30..=37 => gc.fg = n - 30,
            39 => gc.fg = COLOUR_DEFAULT,
            40..=47 => gc.bg = n - 40,
            49 => gc.bg = COLOUR_DEFAULT,
            53 => gc.attr |= GridAttr::OVERLINE,
            55 => gc.attr &= !GridAttr::OVERLINE,
            59 => gc.us = COLOUR_DEFAULT,
            90..=97 => gc.fg = n,
            100..=107 => gc.bg = n - 10,
            _ => {}
        }

        i += 1;
    }
}

/// Handle colon-separated ISO SGR form.
fn dispatch_sgr_colon(gc: &mut super::cell::GridCell, s: &str) {
    let mut parts = [0i32; 8];
    let mut present = [false; 8];
    let mut n = 0;

    for part in s.split(':') {
        if n >= parts.len() {
            return;
        }
        if part.is_empty() {
            present[n] = false;
        } else if let Ok(val) = part.parse::<i32>() {
            if val < 0 {
                return;
            }
            parts[n] = val;
            present[n] = true;
        } else {
            return;
        }
        n += 1;
    }

    if n == 0 {
        return;
    }

    // 4:0..4:5 — underscore styles.
    if present[0] && parts[0] == 4 {
        if n != 2 {
            return;
        }
        let style = if present[1] { parts[1] } else { return };
        gc.attr &= !GridAttr::ALL_UNDERSCORE;
        match style {
            0 => {} // Remove all underscores (already cleared).
            1 => gc.attr |= GridAttr::UNDERSCORE,
            2 => gc.attr |= GridAttr::UNDERSCORE_2,
            3 => gc.attr |= GridAttr::UNDERSCORE_3,
            4 => gc.attr |= GridAttr::UNDERSCORE_4,
            5 => gc.attr |= GridAttr::UNDERSCORE_5,
            _ => {}
        }
        return;
    }

    // 38:2:r:g:b or 48:2:r:g:b or 58:2:r:g:b (also 38:2:_:r:g:b with colour space ID).
    if !present[0] || n < 2 || (parts[0] != 38 && parts[0] != 48 && parts[0] != 58) {
        return;
    }
    if !present[1] {
        return;
    }
    match parts[1] {
        2 => {
            if n < 3 {
                return;
            }
            // Determine if there's a colour-space-id parameter.
            let idx = if n == 5 { 2 } else { 3 };
            if n < idx + 3 {
                return;
            }
            let r = if present[idx] { parts[idx] } else { return };
            let g = if present[idx + 1] {
                parts[idx + 1]
            } else {
                return;
            };
            let b = if present[idx + 2] {
                parts[idx + 2]
            } else {
                return;
            };
            if r > 255 || g > 255 || b > 255 {
                return;
            }
            let c = colour_join_rgb(r as u8, g as u8, b as u8);
            match parts[0] {
                38 => gc.fg = c,
                48 => gc.bg = c,
                58 => gc.us = c,
                _ => {}
            }
        }
        5 => {
            if n < 3 {
                return;
            }
            let c = if present[2] { parts[2] } else { return };
            if c > 255 {
                return;
            }
            match parts[0] {
                38 => gc.fg = c | COLOUR_FLAG_256,
                48 => gc.bg = c | COLOUR_FLAG_256,
                58 => gc.us = c | COLOUR_FLAG_256,
                _ => {}
            }
        }
        _ => {}
    }
}
