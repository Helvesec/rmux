use super::*;

#[tokio::test]
async fn a_window_killed_in_a_group_rebuilds_the_survivors_from_its_first_member() {
    // Measured on tmux 3.7b (S2375 `linked5.sh` case 1, these names). Killing
    // ONE window of a group detaches it from the group's first member by name
    // (`server_kill_window` walks the sessions by name), then synchronises the
    // group from that member (`session.c:340-358`): every OTHER member's
    // winlinks are emptied and created again (`:612-626`, `:633-689`), each
    // appended at the tail of its own window's list. The surviving window's
    // outsider keeps its place and the creator moves behind the peer.
    let handler = RequestHandler::new();
    let outer = created(&handler, "wgi-detach-E").await;
    let creator = created(&handler, "wgi-detach-zulu").await;
    opened(&handler, &creator, 1).await;
    linked(&handler, window(&creator, 0), window(&outer, 1), false).await;
    let peer = joined(&handler, "wgi-detach-alpha", &creator).await;
    assert_eq!(
        names(&handler, window(&peer, 0)).await,
        format!("{},{},{}", creator.as_str(), outer.as_str(), peer.as_str())
    );
    // The OTHER window of the group goes, from the member that joined.
    killed_window(&handler, window(&peer, 1)).await;
    assert_eq!(
        names(&handler, window(&peer, 0)).await,
        format!("{},{},{}", outer.as_str(), peer.as_str(), creator.as_str())
    );
    shutdown(&handler).await;
}

#[tokio::test]
async fn the_walked_accessor_and_the_family_list_do_not_move() {
    // What `handler_window` walks to reconcile, emit and refresh, what the
    // attach resize policy reads, and what the aggregators read are the readings
    // they always were. Only the presentation accessor answers the order, and
    // only inside the explicit-link branch.
    let handler = RequestHandler::new();
    let world = intercalated(&handler, "wgi-walked").await;
    let state = handler.state.lock().await;
    let expected = vec![
        world.creator.as_str().to_owned(),
        world.peer.as_str().to_owned(),
        world.outer.as_str().to_owned(),
    ];
    assert_eq!(
        helpers(&state, &window(&world.peer, 0)),
        (expected.clone(), expected.clone(), 3)
    );
    assert_eq!(
        helpers(&state, &window(&world.outer, 1)),
        (expected.clone(), expected, 3)
    );
    drop(state);
    shutdown(&handler).await;
}

// Measured with tmux 3.7b (S2375 `linked5.sh` cases 2 to 5): both outside
// slots print E,E after unlink-window, for either ordering of the two group
// member names; the owner is the source, so the peer's links are rebuilt after
// it - after the outsider's too when the group also holds the window.
#[tokio::test]
async fn partial_group_removal_preserves_outside_alias_order() {
    for (owner, peer) in [("zulu", "alpha"), ("alpha", "zulu")] {
        for duplicate_in_group in [false, true] {
            let handler = RequestHandler::new();
            let outer = created(&handler, "E").await;
            let creator = created(&handler, owner).await;
            opened(&handler, &creator, 1).await;
            for index in [1, 2] {
                linked(&handler, window(&creator, 0), window(&outer, index), false).await;
            }
            if duplicate_in_group {
                linked(&handler, window(&creator, 0), window(&creator, 2), false).await;
            }
            let peer = joined(&handler, peer, &creator).await;
            unlinked(&handler, window(&creator, 0)).await;
            let known = handler
                .state
                .lock()
                .await
                .sessions
                .window_link_order(&window(&outer, 1));
            let mut expected = vec![outer.clone(), outer.clone()];
            if duplicate_in_group {
                expected.extend([creator.clone(), peer.clone()]);
            }
            let actual = names(&handler, window(&outer, 1)).await;
            let other = names(&handler, window(&outer, 2)).await;
            let b_order = handler
                .state
                .lock()
                .await
                .sessions
                .window_link_order(&window(&peer, 1));
            shutdown(&handler).await;
            assert_eq!(
                known,
                Some(expected),
                "owner={owner}, duplicate={duplicate_in_group}"
            );
            assert_eq!(b_order, Some(vec![creator.clone(), peer.clone()]));
            let outside = if duplicate_in_group {
                format!("E,E,{owner},{}", peer.as_str())
            } else {
                "E,E".to_owned()
            };
            assert_eq!(actual, outside);
            assert_eq!(other, outside);
        }
    }
}

#[tokio::test]
async fn a_renumber_leaves_an_order_this_runtime_cannot_follow() {
    // DEDUCED, not measured, and deliberately NOT implemented. A renumber saves
    // the session's winlink list, empties it and creates every winlink again
    // (`session.c:691-746`), and each new winlink is appended at the TAIL of its
    // window's own list (`window.c:183-193`); the same renumber is then applied
    // to every member of the group. The resulting order is not the one this
    // runtime holds, and this runtime does not model it - so it stops claiming
    // one, and the reader falls back to the family list instead of being told a
    // chronology that is no longer true.
    let handler = RequestHandler::new();
    // The creator's second window is opened BEFORE the JOIN: opening one in a
    // session that is already grouped synchronises the peers, which rebuilds
    // their winlinks and is the very thing the next witness is about. Here the
    // group is formed after, so the order stands until the renumber touches it.
    let outer = created(&handler, "wgi-renumber-E").await;
    let creator = created(&handler, "wgi-renumber-zulu").await;
    opened(&handler, &creator, 9).await;
    linked(&handler, window(&creator, 0), window(&outer, 1), false).await;
    let peer = joined(&handler, "wgi-renumber-alpha", &creator).await;
    assert_eq!(
        names(&handler, window(&peer, 0)).await,
        format!("{},{},{}", creator.as_str(), outer.as_str(), peer.as_str())
    );
    renumbered(&handler, &creator).await;
    assert_eq!(
        names(&handler, window(&peer, 0)).await,
        format!("{},{},{}", creator.as_str(), peer.as_str(), outer.as_str()),
        "the renumber rebuilt the links, so the reader falls back to the family list"
    );
    shutdown(&handler).await;
}
