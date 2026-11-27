use super::*;

#[tokio::test]
async fn popup_repaint_menu_execute_restores_root_and_preserves_nested_popup() {
    for nested in [false, true] {
        for mouse in [false, true] {
            let handler = RequestHandler::new();
            let name = session_name("menu-execute-repaint");
            let pid = 920_245;
            let mut rx = create_quiet_attached_session(&handler, &name, pid).await;
            run_overlay_command(
                &handler,
                pid,
                "set -g status off; set -g automatic-rename off",
            )
            .await;
            if nested {
                run_overlay_command(&handler, pid, "display-popup -N -T Underlying").await;
            }
            run_overlay_command(
                &handler,
                pid,
                "display-menu -M -T Selection Go g 'wait-for -S menu-repaint-done'",
            )
            .await;
            let (rect, popup_id, backlog) = {
                let active_attach = handler.active_attach.lock().await;
                let active = &active_attach.by_pid[&pid];
                let (menu, popup_id) = match active.overlay.as_ref().unwrap() {
                    ClientOverlayState::Menu(menu) => (menu.as_ref(), None),
                    ClientOverlayState::Popup(popup) => {
                        (popup.nested_menu.as_ref().unwrap(), Some(popup.id))
                    }
                };
                (menu.rect, popup_id, Arc::clone(&active.control_backlog))
            };
            let initial = drain_frames(&mut rx, &backlog);
            assert!(initial.last().unwrap().defer_pane_updates);
            let input = if mouse {
                sgr_mouse(3, rect.x + 1, rect.y + 1)
            } else {
                b"g".to_vec()
            };
            handler
                .handle_attached_live_input_for_test(pid, &input)
                .await
                .expect("menu selection executes");
            let mut clear = false;
            let mut refreshed = false;
            let mut popup_redrawn = false;
            while let Ok(control) = rx.try_recv() {
                crate::pane_io::release_attach_control_backlog(
                    &backlog,
                    control.received_backlog_units(),
                );
                match control.materialize_received() {
                    AttachControl::Overlay(frame) if frame.persistent && frame.frame.is_empty() => {
                        assert!(!nested, "a nested menu must not clear its popup");
                        assert!(!frame.defer_pane_updates);
                        clear = true;
                    }
                    AttachControl::Switch(_) => {
                        assert!(!nested, "a nested menu must not expose the base pane");
                        assert!(clear, "persistent clear must precede the base refresh");
                        refreshed = true;
                    }
                    AttachControl::Overlay(frame) if nested => {
                        assert!(frame.persistent && frame.defer_pane_updates);
                        popup_redrawn = true;
                    }
                    _ => {}
                }
            }
            let active_attach = handler.active_attach.lock().await;
            let active = &active_attach.by_pid[&pid];
            if nested {
                let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
                    panic!("nested selection must preserve the popup");
                };
                assert_eq!(Some(popup.id), popup_id);
                assert!(popup.nested_menu.is_none());
                assert!(
                    popup_redrawn,
                    "nested selection must restore the popup frame"
                );
            } else {
                assert!(active.overlay.is_none());
                assert!(
                    clear,
                    "root selection must retire the cached persistent menu"
                );
                assert!(
                    refreshed,
                    "root selection must restore deferred pane output"
                );
            }
        }
    }
}
