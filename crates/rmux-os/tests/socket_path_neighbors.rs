#![cfg(unix)]

use rmux_os::path::socket_paths_match;
use std::os::unix::{fs::symlink, net::UnixListener};
use std::path::Path;

#[test]
fn socket_path_neighbors_preserve_existing_and_unresolved_path_semantics() {
    let root = std::env::temp_dir().join(format!("rmux-sp-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let socket = root.join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    let socket_alias = root.join("socket-alias");
    symlink(&socket, &socket_alias).unwrap();
    let parent_alias = root.join("parent-alias");
    symlink(&root, &parent_alias).unwrap();
    let file = root.join("file");
    std::fs::write(&file, b"existing endpoint spelling").unwrap();

    for (left, right) in [
        (socket.clone(), socket_alias),
        (socket.clone(), parent_alias.join("socket")),
        (file.clone(), root.join(".").join("file")),
        (root.join("missing"), parent_alias.join("missing")),
    ] {
        assert!(socket_paths_match(&left, &right), "{left:?} != {right:?}");
        assert!(socket_paths_match(&right, &left));
    }
    let absent_parent = root.join("absent").join("socket");
    assert!(socket_paths_match(&absent_parent, &absent_parent));
    assert!(!socket_paths_match(&absent_parent, &socket));
    assert!(!socket_paths_match(&socket, &file));
    assert!(socket_paths_match(Path::new(""), Path::new("")));
    assert!(!socket_paths_match(Path::new(""), Path::new(".")));
    assert!(socket_paths_match(Path::new("/"), Path::new("/.")));
    assert!(!socket_paths_match(Path::new("/"), &socket));

    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}
