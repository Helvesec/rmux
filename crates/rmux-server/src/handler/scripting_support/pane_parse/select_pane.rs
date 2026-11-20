//! `select-pane` parsing: direct, adjacent, last-pane and mark forms.

use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::{
    LastPaneRequest, Request, RmuxError, SelectPaneAdjacentRequest, SelectPaneDirection,
    SelectPaneMarkRequest, SelectPaneRequest, WindowTarget,
};

use super::super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::super::{implicit_pane_target, parse_pane_target};

pub(in crate::handler::scripting_support) fn parse_select_pane(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut mark = false;
    let mut clear_marked = false;
    let mut title = None;
    let mut style = None;
    let mut direction = None;
    let mut last = false;
    let mut input_disabled = None;
    let mut preserve_zoom = false;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "select-pane",
                    args.required("-t target")?,
                )?);
            }
            "-m" => {
                let _ = args.optional();
                mark = true;
            }
            "-M" => {
                let _ = args.optional();
                clear_marked = true;
            }
            "-l" => {
                let _ = args.optional();
                last = true;
            }
            "-Z" => {
                let _ = args.optional();
                preserve_zoom = true;
            }
            "-d" => {
                let _ = args.optional();
                // tmux 3.7b gives -e precedence over -d regardless of order.
                input_disabled.get_or_insert(true);
            }
            "-e" => {
                let _ = args.optional();
                input_disabled = Some(false);
            }
            "-T" => {
                let _ = args.optional();
                title = Some(args.required("-T title")?.to_owned());
            }
            "-P" => {
                let _ = args.optional();
                style = Some(args.required("-P style")?.to_owned());
            }
            "-U" => {
                let _ = args.optional();
                direction = Some(SelectPaneDirection::Up);
            }
            "-D" => {
                let _ = args.optional();
                direction = Some(SelectPaneDirection::Down);
            }
            "-L" => {
                let _ = args.optional();
                direction = Some(SelectPaneDirection::Left);
            }
            "-R" => {
                let _ = args.optional();
                direction = Some(SelectPaneDirection::Right);
            }
            _ => {
                // tmux 3.7b accepts the hidden value-taking -P option even though
                // its list-commands signature omits it, so parse compact forms
                // here without changing the public inventory.
                let Some(cluster) =
                    parse_compact_flag_cluster("select-pane", &token, "DdeLlMmRUZ", "PTt")?
                else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('m') => mark = true,
                        CompactFlag::Bare('M') => clear_marked = true,
                        CompactFlag::Bare('l') => last = true,
                        CompactFlag::Bare('Z') => preserve_zoom = true,
                        CompactFlag::Bare('d') => {
                            input_disabled.get_or_insert(true);
                        }
                        CompactFlag::Bare('e') => input_disabled = Some(false),
                        CompactFlag::Bare('U') => direction = Some(SelectPaneDirection::Up),
                        CompactFlag::Bare('D') => direction = Some(SelectPaneDirection::Down),
                        CompactFlag::Bare('L') => direction = Some(SelectPaneDirection::Left),
                        CompactFlag::Bare('R') => direction = Some(SelectPaneDirection::Right),
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_pane_target(
                                "select-pane",
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'T', .. } => {
                            title = Some(compact_flag.value_or_next(&mut args, "-T title")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'P', .. } => {
                            style = Some(compact_flag.value_or_next(&mut args, "-P style")?);
                        }
                        _ => unreachable!("compact select-pane flags are prevalidated"),
                    }
                }
            }
        }
    }
    args.no_extra("select-pane")?;

    if mark && clear_marked {
        return Err(RmuxError::Server(
            "select-pane flags -m and -M cannot be used together".to_owned(),
        ));
    }
    if direction.is_some() && (mark || clear_marked || title.is_some()) {
        return Err(RmuxError::Server(
            "select-pane -U/-D/-L/-R cannot be combined with -m, -M, or -T".to_owned(),
        ));
    }
    if style.is_some() && (direction.is_some() || last || mark || clear_marked) {
        return Err(RmuxError::Server(
            "select-pane -P cannot be combined with -U, -D, -L, -R, -l, -m, or -M".to_owned(),
        ));
    }
    if last && (direction.is_some() || mark || clear_marked) {
        return Err(RmuxError::Server(
            "select-pane -l cannot be combined with -U, -D, -L, -R, -m, or -M".to_owned(),
        ));
    }

    let target = match target {
        Some(target) => target,
        None => implicit_pane_target(sessions, find_context, "select-pane")?,
    };

    if last {
        Ok(Request::LastPane(LastPaneRequest {
            target: WindowTarget::with_window(target.session_name().clone(), target.window_index()),
            preserve_zoom,
            input_disabled,
        }))
    } else if let Some(direction) = direction {
        Ok(Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
            target,
            direction,
            preserve_zoom,
        }))
    } else if mark || clear_marked {
        Ok(Request::SelectPaneMark(SelectPaneMarkRequest {
            target,
            clear: clear_marked,
            title,
        }))
    } else {
        Ok(Request::SelectPane(Box::new(SelectPaneRequest {
            target,
            title,
            style,
            input_disabled,
            preserve_zoom,
        })))
    }
}
