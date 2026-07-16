use rmux_proto::{Request, RmuxError};

use super::parse_pane_target;
use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::unsupported_flag;

pub(super) fn parse_copy_mode(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut source = None;
    let mut page_down = false;
    let mut exit_on_scroll = false;
    let mut hide_position = false;
    let mut mouse_drag_start = false;
    let mut cancel_mode = false;
    let mut scrollbar_scroll = false;
    let mut page_up = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-d" => {
                let _ = args.optional();
                page_down = true;
            }
            "-e" => {
                let _ = args.optional();
                exit_on_scroll = true;
            }
            "-H" => {
                let _ = args.optional();
                hide_position = true;
            }
            "-M" => {
                let _ = args.optional();
                mouse_drag_start = true;
            }
            "-q" => {
                let _ = args.optional();
                cancel_mode = true;
            }
            "-S" => {
                let _ = args.optional();
                scrollbar_scroll = true;
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_pane_target(
                    "copy-mode",
                    args.required("-s src-pane")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target("copy-mode", args.required("-t target")?)?);
            }
            "-u" => {
                let _ = args.optional();
                page_up = true;
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster(token, "deHMSqu", "st") else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('d') => page_down = true,
                        CompactFlag::Bare('e') => exit_on_scroll = true,
                        CompactFlag::Bare('H') => hide_position = true,
                        CompactFlag::Bare('M') => mouse_drag_start = true,
                        CompactFlag::Bare('q') => cancel_mode = true,
                        CompactFlag::Bare('S') => scrollbar_scroll = true,
                        compact_flag @ CompactFlag::Value { flag: 's', .. } => {
                            source = Some(parse_pane_target(
                                "copy-mode",
                                compact_flag.value_or_next(&mut args, "-s src-pane")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_pane_target(
                                "copy-mode",
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        CompactFlag::Bare('u') => page_up = true,
                        CompactFlag::Bare(flag) | CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("copy-mode", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }

    args.no_extra("copy-mode")?;
    Ok(Request::CopyMode(rmux_proto::CopyModeRequest {
        target,
        page_down,
        exit_on_scroll,
        hide_position,
        mouse_drag_start,
        cancel_mode,
        scrollbar_scroll,
        source,
        page_up,
    }))
}

pub(super) fn parse_clock_mode(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "clock-mode",
                    args.required("-t target")?,
                )?);
            }
            _ => break,
        }
    }

    args.no_extra("clock-mode")?;
    Ok(Request::ClockMode(rmux_proto::ClockModeRequest { target }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: &str) -> String {
        value.to_owned()
    }

    #[test]
    fn parse_copy_mode_accepts_tmux_compact_hidden_target() {
        let request = parse_copy_mode(CommandTokens::new(vec![token("-Ht=")]))
            .expect("compact hidden-position target parses");

        let Request::CopyMode(request) = request else {
            panic!("compact target must parse as copy-mode request");
        };
        assert!(request.hide_position);
        assert_eq!(
            request.target,
            Some(parse_pane_target("copy-mode", "=".to_owned()).unwrap())
        );
    }
}
