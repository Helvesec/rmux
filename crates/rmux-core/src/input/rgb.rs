use super::colour::{
    Colour, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB, COLOUR_NONE, COLOUR_TERMINAL,
};

/// Resolve an indexed or true colour using the tmux palette.
#[must_use]
pub fn colour_to_rgb(colour: Colour) -> Option<(u8, u8, u8)> {
    if matches!(colour, COLOUR_NONE | COLOUR_DEFAULT | COLOUR_TERMINAL) {
        return None;
    }
    if (colour & COLOUR_FLAG_RGB) != 0 {
        return Some((
            ((colour >> 16) & 0xff) as u8,
            ((colour >> 8) & 0xff) as u8,
            (colour & 0xff) as u8,
        ));
    }
    if (colour & COLOUR_FLAG_256) != 0 {
        return xterm_palette_rgb((colour & 0xff) as u8);
    }

    match colour {
        0..=7 => TMUX_COLOUR_PALETTE.get(colour as usize).copied(),
        90..=97 => TMUX_COLOUR_PALETTE.get((colour - 82) as usize).copied(),
        _ => None,
    }
}

fn xterm_palette_rgb(index: u8) -> Option<(u8, u8, u8)> {
    if usize::from(index) < TMUX_COLOUR_PALETTE.len() {
        return TMUX_COLOUR_PALETTE.get(index as usize).copied();
    }
    if (16..=231).contains(&index) {
        let cube = index - 16;
        let red = cube / 36;
        let green = (cube % 36) / 6;
        let blue = cube % 6;
        return Some((
            CUBE_LEVELS[red as usize],
            CUBE_LEVELS[green as usize],
            CUBE_LEVELS[blue as usize],
        ));
    }
    if (232..=255).contains(&index) {
        let value = 8 + 10 * (index - 232);
        return Some((value, value, value));
    }
    None
}

const TMUX_COLOUR_PALETTE: &[(u8, u8, u8)] = &[
    (0, 0, 0),
    (128, 0, 0),
    (0, 128, 0),
    (128, 128, 0),
    (0, 0, 128),
    (128, 0, 128),
    (0, 128, 128),
    (192, 192, 192),
    (128, 128, 128),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (0, 0, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

const CUBE_LEVELS: &[u8] = &[0, 95, 135, 175, 215, 255];
