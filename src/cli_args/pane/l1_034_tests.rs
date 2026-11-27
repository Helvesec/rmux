use super::*;
use crate::cli_args::{parse, Command};
#[test]
fn l1_034_sequential_height_error_preserves_width_and_strict_structural_validation() {
    for arguments in [
        vec!["-x10", "-ybad"],
        vec!["-ybad", "-x10"],
        vec!["-x", "10", "-y", "-1"],
        vec!["-x25%", "-y2147483648"],
    ] {
        let mut argv = vec!["rmux", "resize-pane"];
        argv.extend(arguments);
        let cli = parse(argv).unwrap();
        let Command::ResizePane(args) = cli.command.unwrap() else {
            panic!("resize");
        };
        assert!(args.columns.is_some());
        assert!(matches!(args.rows, Some(ResizePaneSize::DeferredError(_))));
    }
    for arguments in [
        vec!["-ybad"],
        vec!["-y-1"],
        vec!["-xbad", "-y3"],
        vec!["-x10", "-ybad", "-X2"],
        vec!["-x10", "-y3", "-Y2"],
    ] {
        let mut argv = vec!["rmux", "resize-pane"];
        argv.extend(arguments);
        assert!(parse(argv).is_err());
    }
}
#[test]
fn l1_034_explicit_snapshot_v2_queue_retains_v1_arguments() {
    for (command, v2) in [
        ("export-window-snapshot", false),
        ("export-window-snapshot-v2", true),
    ] {
        let c = parse(["rmux", command, "-t", "sample:0"])
            .unwrap()
            .command
            .unwrap();
        match c {
            Command::ExportWindowSnapshot(a) => {
                assert!(!v2);
                assert!(a.target.is_some());
            }
            Command::ExportWindowSnapshotV2(a) => {
                assert!(v2);
                assert!(a.target.is_some());
            }
            _ => panic!("export route"),
        }
    }
    for file in ["-", "snapshot.json"] {
        let c = parse(["rmux", "restore-window-snapshot-v2", "-t", "sample:0", file])
            .unwrap()
            .command
            .unwrap();
        let Command::RestoreWindowSnapshotV2(a) = c else {
            panic!("restore route")
        };
        assert_eq!(a.snapshot_path, std::path::PathBuf::from(file));
    }
}
