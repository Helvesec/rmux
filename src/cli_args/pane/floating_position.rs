use super::ResizePaneArgs;

pub(super) fn validate_occurrences(arguments: &[String]) -> Result<bool, clap::Error> {
    let (x, y) = position_occurrences(arguments);
    if x > 1 || y > 1 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "resize-pane position accepts each of -X and -Y at most once",
        ));
    }
    Ok(x > 0 || y > 0)
}

pub(super) fn parse_value(value: &str) -> Result<i32, String> {
    if value.ends_with('%') {
        return Err("position does not accept percentages".to_owned());
    }
    let position = value
        .parse::<i128>()
        .map_err(|error| format!("invalid position: {error}"))?;
    i32::try_from(position).map_err(|_| "position outside signed 32-bit range".to_owned())
}

pub(super) fn validate_mode(args: &ResizePaneArgs) -> Result<bool, clap::Error> {
    if args.position_x.is_none() && args.position_y.is_none() {
        return Ok(false);
    }
    let mixed_with_resize = args.down.is_some()
        || args.up.is_some()
        || args.left.is_some()
        || args.right.is_some()
        || args.columns.is_some()
        || args.rows.is_some()
        || args.zoom
        || args.mouse
        || args.trim_below;
    if mixed_with_resize {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "resize-pane -X/-Y cannot be combined with resize, mouse, trim, or zoom actions",
        ));
    }
    Ok(true)
}

fn position_occurrences(arguments: &[String]) -> (usize, usize) {
    let Some(spec) = rmux_core::command_inventory::command_short_option_spec("resize-pane") else {
        return (0, 0);
    };
    let (mut x, mut y) = (0, 0);
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" || argument == "-" || !argument.starts_with('-') {
            break;
        }
        let Some(flags) = argument
            .strip_prefix('-')
            .filter(|flags| !flags.starts_with('-'))
        else {
            break;
        };
        let mut chars = flags.chars().peekable();
        while let Some(flag) = chars.next() {
            match flag {
                'X' => x += 1,
                'Y' => y += 1,
                _ => {}
            }
            if spec.takes_value(flag) {
                if chars.peek().is_none() {
                    index += 1;
                }
                break;
            }
            if !spec.is_boolean(flag) {
                break;
            }
        }
        index += 1;
    }
    (x, y)
}
