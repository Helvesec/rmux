use super::{layout_checksum, LayoutTree};
use crate::PaneId;

fn encoded(body: &str) -> String {
    format!("{:04x},{body}", layout_checksum(body))
}

#[test]
fn persistence_import_checks_dimensions_before_pruning_saved_panes() {
    let ids = [PaneId::new(41), PaneId::new(99)];
    // tmux 3.7b accepts zero-sized leaves and split children. They do not
    // overflow signed resize deltas and must retain their existing behavior.
    for (body, count) in [
        ("0x30,0,0,0", 1),
        ("100x0,0,0,0", 1),
        ("0x0,0,0,0", 1),
        ("1x30,0,0{0x30,0,0,0,0x30,1,0,1}", 2),
        ("100x1,0,0[100x0,0,0,0,100x0,0,1,1]", 2),
    ] {
        assert!(LayoutTree::parse(&encoded(body), &ids[..count]).is_ok());
    }
    let valid = "100x30,0,0{50x30,0,0,0,49x30,51,0,1}";
    for count in [2, 1] {
        let expected = LayoutTree::parse(&encoded(valid), &ids[..count]).unwrap();
        // A saved root's dimensions are derived from its children. Preserve
        // this historical normalization, including when an extra pane is pruned.
        for width in [0, 1, 101, u32::MAX] {
            let body = valid.replacen("100x30", &format!("{width}x30"), 1);
            assert_eq!(
                LayoutTree::parse(&encoded(&body), &ids[..count]).unwrap(),
                expected
            );
        }
        for damaged in [
            "100x30,0,0{4294967295x30,0,0,0,49x30,51,0,1}",
            "100x30,0,0[100x4294967295,0,0,0,100x15,0,15,1]",
            "100x30,0,0{2147483647x30,0,0,0,2147483647x30,0,0,1}",
            "100x30,0,0{50x30,0,0[50x13,0,0,0,50x15,0,15,1],49x30,51,0,2}",
        ] {
            assert!(
                LayoutTree::parse(&encoded(damaged), &ids[..count]).is_err(),
                "damaged saved layout must be rejected with {count} surviving panes: {damaged}"
            );
        }
    }
}
