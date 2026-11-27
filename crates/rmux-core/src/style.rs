//! tmux-compatible style and colour parsing.
//!
//! This module owns the shared style grammar used by options, format style
//! clauses, and renderer-facing colour parsing. The parser is additive: it
//! mutates an existing [`Style`] using a base [`StyleCell`] for `default`
//! resets, matching tmux `style_parse`.

use std::fmt;

use crate::input::{Colour, GridAttr, COLOUR_DEFAULT};

mod colour;
mod grammar;
mod types;

pub use colour::{colour_to_string, parse_colour, ColourParseError};
pub use types::{
    Style, StyleAlign, StyleCell, StyleDefaultType, StyleList, StyleRange, StyleWidth,
};

use grammar::{attributes_to_string, parse_attributes, parse_range, parse_width, strip_prefix_ci};

const STYLE_TOKEN_DELIMITERS: &[char] = &[' ', ',', '\n'];

impl Style {
    /// Parses a standalone style string using tmux's default base cell.
    pub fn parse(input: &str) -> Result<Self, StyleParseError> {
        let mut style = Self::default();
        style.parse_in_place(&StyleCell::default(), input)?;
        Ok(style)
    }

    /// Creates a style whose cell state starts from `cell`.
    #[must_use]
    pub fn with_cell(cell: StyleCell) -> Self {
        Self {
            cell,
            ..Self::default()
        }
    }

    /// Applies `input` onto the current style using `base` for `default`
    /// resets.
    pub fn parse_in_place(&mut self, base: &StyleCell, input: &str) -> Result<(), StyleParseError> {
        if input.is_empty() {
            return Ok(());
        }

        let saved = self.clone();
        for token in input
            .split(|character| STYLE_TOKEN_DELIMITERS.contains(&character))
            .filter(|token| !token.is_empty())
        {
            if let Err(error) = self.apply_token(base, token) {
                *self = saved;
                return Err(error);
            }
        }
        Ok(())
    }

    /// Returns a parsed copy of the current style.
    pub fn applied(&self, base: &StyleCell, input: &str) -> Result<Self, StyleParseError> {
        let mut next = self.clone();
        next.parse_in_place(base, input)?;
        Ok(next)
    }

    /// Returns a parsed copy of the current style using the current cell as the
    /// additive base for `default` resets.
    pub fn overlaid(&self, input: &str) -> Result<Self, StyleParseError> {
        self.applied(&self.cell, input)
    }

    fn apply_token(&mut self, base: &StyleCell, token: &str) -> Result<(), StyleParseError> {
        let lowered = token.to_ascii_lowercase();

        // Exact keyword matches (case-insensitive).
        match lowered.as_str() {
            "default" => {
                self.cell = *base;
                return Ok(());
            }
            "ignore" => {
                self.ignore = true;
                return Ok(());
            }
            "noignore" => {
                self.ignore = false;
                return Ok(());
            }
            "push-default" => {
                self.default_type = StyleDefaultType::Push;
                return Ok(());
            }
            "pop-default" => {
                self.default_type = StyleDefaultType::Pop;
                return Ok(());
            }
            "set-default" => {
                self.default_type = StyleDefaultType::Set;
                return Ok(());
            }
            "nolist" => {
                self.list = StyleList::Off;
                return Ok(());
            }
            "norange" => {
                self.range = StyleRange::None;
                return Ok(());
            }
            "noalign" => {
                self.align = StyleAlign::Default;
                return Ok(());
            }
            "none" => {
                self.cell.attr = 0;
                return Ok(());
            }
            _ => {}
        }

        // Prefixed key=value directives.  `strip_prefix_ci` preserves the
        // original casing of the value portion (needed for colour hex digits).
        if let Some(value) = strip_prefix_ci(token, "list=") {
            self.list = match value.to_ascii_lowercase().as_str() {
                "on" => StyleList::On,
                "focus" => StyleList::Focus,
                "left-marker" => StyleList::LeftMarker,
                "right-marker" => StyleList::RightMarker,
                _ => return Err(StyleParseError::invalid(token)),
            };
            return Ok(());
        }

        if let Some(value) = strip_prefix_ci(token, "range=") {
            self.range = parse_range(value).ok_or_else(|| StyleParseError::invalid(token))?;
            return Ok(());
        }

        if let Some(value) = strip_prefix_ci(token, "align=") {
            self.align = match value.to_ascii_lowercase().as_str() {
                "left" => StyleAlign::Left,
                "centre" => StyleAlign::Centre,
                "right" => StyleAlign::Right,
                "absolute-centre" => StyleAlign::AbsoluteCentre,
                _ => return Err(StyleParseError::invalid(token)),
            };
            return Ok(());
        }

        if let Some(value) = strip_prefix_ci(token, "fill=") {
            self.fill = parse_colour(value).map_err(|_| StyleParseError::invalid(token))?;
            return Ok(());
        }

        if let Some(value) = strip_prefix_ci(token, "fg=") {
            self.cell.fg = resolve_style_colour(base.fg, value, token)?;
            return Ok(());
        }

        if let Some(value) = strip_prefix_ci(token, "bg=") {
            self.cell.bg = resolve_style_colour(base.bg, value, token)?;
            return Ok(());
        }

        if let Some(value) = strip_prefix_ci(token, "us=") {
            self.cell.us = resolve_style_colour(base.us, value, token)?;
            return Ok(());
        }

        if let Some(value) = strip_prefix_ci(token, "width=") {
            self.width = Some(parse_width(value).ok_or_else(|| StyleParseError::invalid(token))?);
            return Ok(());
        }

        if let Some(value) = strip_prefix_ci(token, "pad=") {
            self.pad = Some(
                value
                    .parse::<u32>()
                    .map_err(|_| StyleParseError::invalid(token))?,
            );
            return Ok(());
        }

        // Attribute negation: "noattr" sets the NOATTR sentinel,
        // "no<attr>" clears the named attribute bit.
        if let Some(attr_name) = lowered.strip_prefix("no") {
            if attr_name == "attr" {
                self.cell.attr |= GridAttr::NOATTR;
                return Ok(());
            }
            let bits =
                parse_attributes(attr_name).ok_or_else(|| StyleParseError::invalid(token))?;
            self.cell.attr &= !bits;
            return Ok(());
        }

        // Bare colour as implicit fg (uses original token for hex casing).
        if let Ok(colour) = parse_colour(token) {
            self.cell.fg = if colour == COLOUR_DEFAULT {
                base.fg
            } else {
                colour
            };
            return Ok(());
        }

        // Bare attribute name.
        let bits = parse_attributes(token).ok_or_else(|| StyleParseError::invalid(token))?;
        self.cell.attr |= bits;
        Ok(())
    }
}

/// Parses `input` onto `style`, matching tmux `style_parse`.
pub fn style_parse(
    style: &mut Style,
    base: &StyleCell,
    input: &str,
) -> Result<(), StyleParseError> {
    style.parse_in_place(base, input)
}

/// Returns the canonical tmux string form for `style`.
#[must_use]
pub fn style_tostring(style: &Style) -> String {
    let mut tokens = Vec::new();

    if let Some(list) = style.list.as_tmux_str() {
        tokens.push(format!("list={list}"));
    }
    if let Some(range) = style.range.as_tmux_value() {
        tokens.push(format!("range={range}"));
    }
    if let Some(align) = style.align.as_tmux_str() {
        tokens.push(format!("align={align}"));
    }
    if let Some(default_type) = style.default_type.as_tmux_str() {
        tokens.push(default_type.to_owned());
    }
    if style.fill != COLOUR_DEFAULT {
        tokens.push(format!("fill={}", colour_to_string(style.fill)));
    }
    if style.cell.fg != COLOUR_DEFAULT {
        tokens.push(format!("fg={}", colour_to_string(style.cell.fg)));
    }
    if style.cell.bg != COLOUR_DEFAULT {
        tokens.push(format!("bg={}", colour_to_string(style.cell.bg)));
    }
    if style.cell.us != COLOUR_DEFAULT {
        tokens.push(format!("us={}", colour_to_string(style.cell.us)));
    }
    if style.cell.attr != 0 {
        tokens.push(attributes_to_string(style.cell.attr));
    }
    if let Some(width) = style.width {
        tokens.push(format!("width={}", width.as_tmux_value()));
    }
    if let Some(pad) = style.pad {
        tokens.push(format!("pad={pad}"));
    }

    if tokens.is_empty() {
        "default".to_owned()
    } else {
        tokens.join(",")
    }
}

fn resolve_style_colour(base: Colour, value: &str, token: &str) -> Result<Colour, StyleParseError> {
    let colour = parse_colour(value).map_err(|_| StyleParseError::invalid(token))?;
    Ok(if colour == COLOUR_DEFAULT {
        base
    } else {
        colour
    })
}

/// Style parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleParseError {
    token: String,
}

impl StyleParseError {
    fn invalid(token: &str) -> Self {
        Self {
            token: token.to_owned(),
        }
    }
}

impl fmt::Display for StyleParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid style token: {}", self.token)
    }
}

impl std::error::Error for StyleParseError {}

#[cfg(test)]
mod tests {
    use super::{
        colour_to_string, parse_colour, style_parse, style_tostring, ColourParseError, Style,
        StyleAlign, StyleCell, StyleDefaultType, StyleList, StyleRange, StyleWidth,
    };
    use crate::input::{
        colour_join_rgb, GridAttr, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_NONE, COLOUR_TERMINAL,
    };

    fn default_base() -> StyleCell {
        StyleCell::default()
    }

    fn parse_style(input: &str) -> Style {
        Style::parse(input).expect("style parses")
    }

    #[test]
    fn colour_parser_accepts_all_supported_forms() {
        for (input, expected) in [
            ("black", 0),
            ("red", 1),
            ("green", 2),
            ("yellow", 3),
            ("blue", 4),
            (concat!("mag", "enta"), 5),
            ("cyan", 6),
            ("white", 7),
            ("brightblack", 90),
            ("brightred", 91),
            ("brightgreen", 92),
            ("brightyellow", 93),
            ("brightblue", 94),
            (concat!("bright", "mag", "enta"), 95),
            ("brightcyan", 96),
            ("brightwhite", 97),
            ("colour214", COLOUR_FLAG_256 | 214),
            ("color33", COLOUR_FLAG_256 | 33),
            ("214", COLOUR_FLAG_256 | 214),
            ("default", COLOUR_DEFAULT),
            ("terminal", COLOUR_TERMINAL),
            ("none", COLOUR_NONE),
        ] {
            assert_eq!(parse_colour(input), Ok(expected), "{input}");
        }
        assert_eq!(
            parse_colour("#123456"),
            Ok(colour_join_rgb(0x12, 0x34, 0x56))
        );
        assert_eq!(
            parse_colour("colour256"),
            Err(ColourParseError::Invalid("colour256".to_owned()))
        );
    }

    #[test]
    fn colour_tostring_canonicalizes_supported_forms() {
        assert_eq!(colour_to_string(COLOUR_NONE), "none");
        assert_eq!(colour_to_string(COLOUR_DEFAULT), "default");
        assert_eq!(colour_to_string(COLOUR_TERMINAL), "terminal");
        assert_eq!(colour_to_string(COLOUR_FLAG_256 | 214), "colour214");
        assert_eq!(
            colour_to_string(colour_join_rgb(0x12, 0x34, 0x56)),
            "#123456"
        );
    }

    #[test]
    fn style_parser_accepts_attributes_none_noattr_and_negation() {
        let style = parse_style(
            "acs,bold,bright,dim,underscore,blink,reverse,hidden,italics,\
             strikethrough,double-underscore,curly-underscore,dotted-underscore,\
             dashed-underscore,overline,noattr",
        );
        assert_eq!(
            style.cell.attr,
            GridAttr::CHARSET
                | GridAttr::BRIGHT
                | GridAttr::DIM
                | GridAttr::UNDERSCORE
                | GridAttr::BLINK
                | GridAttr::REVERSE
                | GridAttr::HIDDEN
                | GridAttr::ITALICS
                | GridAttr::STRIKETHROUGH
                | GridAttr::UNDERSCORE_2
                | GridAttr::UNDERSCORE_3
                | GridAttr::UNDERSCORE_4
                | GridAttr::UNDERSCORE_5
                | GridAttr::OVERLINE
                | GridAttr::NOATTR
        );

        let style = parse_style("bold,reverse,nobold,noreverse");
        assert_eq!(style.cell.attr, 0);

        let style = parse_style("none");
        assert_eq!(style.cell.attr, 0);
    }

    #[test]
    fn style_parser_accepts_prefixed_and_bare_colours() {
        let style = parse_style("fg=red,bg=colour214,us=#123456,fill=214,blue");
        assert_eq!(style.cell.fg, 4);
        assert_eq!(style.cell.bg, COLOUR_FLAG_256 | 214);
        assert_eq!(style.cell.us, colour_join_rgb(0x12, 0x34, 0x56));
        assert_eq!(style.fill, COLOUR_FLAG_256 | 214);
    }

    #[test]
    fn style_parser_accepts_list_align_range_width_pad_and_defaults() {
        let style = parse_style(
            "list=focus,align=absolute-centre,range=window|42,width=50%,pad=3,push-default,ignore",
        );
        assert_eq!(style.list, StyleList::Focus);
        assert_eq!(style.align, StyleAlign::AbsoluteCentre);
        assert_eq!(style.range, StyleRange::Window(42));
        assert_eq!(style.width, Some(StyleWidth::Percentage(50)));
        assert_eq!(style.pad, Some(3));
        assert_eq!(style.default_type, StyleDefaultType::Push);
        assert!(style.ignore);
    }

    #[test]
    fn style_parser_is_case_insensitive_and_accepts_space_and_newline_delimiters() {
        let style = parse_style(
            "FG=RED BG=blue\nUs=#123456 Fill=214 BOLD REVERSE LIST=FOCUS RANGE=PANE|%7",
        );
        assert_eq!(style.cell.fg, 1);
        assert_eq!(style.cell.bg, 4);
        assert_eq!(style.cell.us, colour_join_rgb(0x12, 0x34, 0x56));
        assert_eq!(style.fill, COLOUR_FLAG_256 | 214);
        assert_eq!(style.cell.attr, GridAttr::BRIGHT | GridAttr::REVERSE);
        assert_eq!(style.list, StyleList::Focus);
        assert_eq!(style.range, StyleRange::Pane(7));
    }

    #[test]
    fn style_parser_validates_all_range_variants() {
        for input in [
            "range=left",
            "range=right",
            "range=pane|%7",
            "range=window|9",
            "range=session|$11",
            "range=user|custom-tag",
            "range=control|9",
        ] {
            parse_style(input);
        }

        for input in [
            "range=left|1",
            "range=pane|7",
            "range=session|11",
            "range=control|10",
            "range=user|",
            "range=user|0123456789abcdef",
            "range=window|x",
        ] {
            let mut style = Style::default();
            assert!(
                style_parse(&mut style, &default_base(), input).is_err(),
                "{input}"
            );
            assert_eq!(style, Style::default(), "{input}");
        }
    }

    #[test]
    fn style_parser_supports_norange_nolist_noalign_and_noignore() {
        let mut style =
            parse_style("list=left-marker,align=right,range=right,ignore,width=3,pad=2");
        style_parse(
            &mut style,
            &default_base(),
            "nolist,noalign,norange,noignore",
        )
        .expect("second parse succeeds");
        assert_eq!(style.list, StyleList::Off);
        assert_eq!(style.align, StyleAlign::Default);
        assert_eq!(style.range, StyleRange::None);
        assert!(!style.ignore);
        assert_eq!(style.width, Some(StyleWidth::Cells(3)));
        assert_eq!(style.pad, Some(2));
    }

    #[test]
    fn style_parser_is_additive_and_default_resets_to_base_cell() {
        let base = parse_style("fg=red,bg=blue,bold");
        let style = base.overlaid("bg=green,reverse").expect("overlay parses");
        assert_eq!(style.cell.fg, 1);
        assert_eq!(style.cell.bg, 2);
        assert_eq!(style.cell.attr, GridAttr::BRIGHT | GridAttr::REVERSE);

        let reset = style
            .applied(&base.cell, "default")
            .expect("default parses");
        assert_eq!(reset.cell, base.cell);
        assert_eq!(reset.fill, style.fill);
    }

    #[test]
    fn empty_style_string_is_a_no_op() {
        let style = parse_style("fg=red");
        let next = style.overlaid("").expect("empty parse succeeds");
        assert_eq!(next, style);
    }

    #[test]
    fn invalid_style_keeps_the_original_value() {
        let original = parse_style("fg=red,bold");
        let mut style = original.clone();
        assert!(style_parse(&mut style, &default_base(), "fg=invalid").is_err());
        assert_eq!(style, original);
    }

    #[test]
    fn non_ascii_tokens_are_invalid_styles_instead_of_panics() {
        // A multi-byte character that straddles a directive prefix length
        // must be reported like tmux's "invalid style", never sliced.
        let mut tokens = vec!["ab\u{e9}".to_owned(), "abcde\u{e9}".to_owned()];
        for prefix in [
            "list=", "range=", "align=", "fill=", "fg=", "bg=", "us=", "width=", "pad=", "colour",
            "color",
        ] {
            tokens.push(format!("{}\u{e9}", &prefix[..prefix.len() - 1]));
            tokens.push(format!("{prefix}\u{e9}"));
        }
        for token in tokens {
            assert!(Style::parse(&token).is_err(), "{token:?} must be invalid");
            assert!(parse_colour(&token).is_err(), "{token:?} is not a colour");
        }
    }

    #[test]
    fn style_tostring_round_trips_supported_states() {
        let cases = [
            "default",
            "fg=red,bg=blue",
            "fill=#123456,fg=214,bg=none,us=colour33,bold,reverse,width=42,pad=0",
            "list=left-marker,range=left,align=centre,push-default,ignore",
            "range=pane|%9,list=focus,fg=none,noattr",
            "range=session|$5,list=on,align=absolute-centre,width=50%",
            "range=user|custom,list=right-marker,pop-default",
            "range=control|7,set-default",
        ];

        for input in cases {
            let style = parse_style(input);
            let rendered = style_tostring(&style);
            let mut round_tripped = Style::default();
            style_parse(&mut round_tripped, &default_base(), &rendered)
                .expect("rendered style parses");
            if style.ignore {
                let mut expected = style.clone();
                expected.ignore = false;
                assert_eq!(round_tripped, expected, "{input} => {rendered}");
            } else {
                assert_eq!(round_tripped, style, "{input} => {rendered}");
            }
        }
    }

    #[test]
    fn style_tostring_emits_default_for_empty_style() {
        assert_eq!(style_tostring(&Style::default()), "default");
    }

    #[test]
    fn style_tostring_omits_ignore_like_tmux() {
        let style = parse_style("fg=red,ignore");
        assert_eq!(style_tostring(&style), "fg=red");
    }
}
