use super::*;

#[test]
fn empty_store() {
    let store = BufferStore::new();
    assert!(store.is_empty());
    assert_eq!(store.len(), 0);
    assert_eq!(store.stack_head(), None);
}

#[test]
fn set_unnamed_buffer_creates_deterministic_names() {
    let mut store = BufferStore::new();
    let outcome = store.set(None, b"hello".to_vec(), 50).unwrap();
    assert_eq!(outcome.buffer_name(), Some("buffer0"));
    assert!(outcome.evicted().is_empty());

    let outcome = store.set(None, b"world".to_vec(), 50).unwrap();
    assert_eq!(outcome.buffer_name(), Some("buffer1"));

    assert_eq!(store.len(), 2);
    assert_eq!(store.stack_head(), Some("buffer1"));
    assert_eq!(store.top_unnamed(), Some("buffer1"));
}

#[test]
fn set_named_buffer_creates_and_replaces() {
    let mut store = BufferStore::new();
    store.set(Some("my-buf"), b"v1".to_vec(), 50).unwrap();
    assert_eq!(store.get("my-buf"), Some(b"v1".as_slice()));

    store
        .set(Some("my-buf"), b"value-two".to_vec(), 50)
        .unwrap();
    assert_eq!(store.get("my-buf"), Some(b"value-two".as_slice()));
    assert_eq!(store.len(), 1);
    assert_eq!(store.stack_head(), Some("my-buf"));
}

#[test]
fn stack_head_tracks_most_recent_operation() {
    let mut store = BufferStore::new();
    store.set(Some("alpha"), b"a".to_vec(), 50).unwrap();
    store.set(None, b"b".to_vec(), 50).unwrap();
    assert_eq!(store.stack_head(), Some("buffer0"));

    store.set(Some("alpha"), b"a2".to_vec(), 50).unwrap();
    assert_eq!(store.stack_head(), Some("alpha"));
}

#[test]
fn fifo_eviction_removes_oldest_unnamed_buffers() {
    let mut store = BufferStore::new();
    for _ in 0..3 {
        store.set(None, b"x".to_vec(), 50).unwrap();
    }
    let outcome = store.set(None, b"new".to_vec(), 3).unwrap();
    assert_eq!(outcome.buffer_name(), Some("buffer3"));
    assert_eq!(outcome.evicted(), &["buffer0"]);
    assert_eq!(store.len(), 3);
}

#[test]
fn named_buffers_are_exempt_from_fifo_eviction() {
    let mut store = BufferStore::new();
    store.set(Some("named"), b"keep".to_vec(), 50).unwrap();
    store.set(None, b"u0".to_vec(), 50).unwrap();
    store.set(None, b"u1".to_vec(), 50).unwrap();

    let outcome = store.set(None, b"u2".to_vec(), 2).unwrap();
    assert_eq!(outcome.evicted(), &["buffer0"]);
    assert!(store.get("named").is_some());
    assert_eq!(store.len(), 3);
}

#[test]
fn delete_stack_head_when_no_name_provided() {
    let mut store = BufferStore::new();
    store.set(None, b"a".to_vec(), 50).unwrap();
    store.set(None, b"b".to_vec(), 50).unwrap();
    let deleted = store.delete(None).unwrap();
    assert_eq!(deleted, "buffer1");
    assert_eq!(store.len(), 1);
}

#[test]
fn delete_without_name_ignores_newer_named_buffers() {
    let mut store = BufferStore::new();
    store.set(None, b"auto".to_vec(), 50).unwrap();
    store.set(Some("named"), b"manual".to_vec(), 50).unwrap();

    let deleted = store.delete(None).unwrap();
    assert_eq!(deleted, "buffer0");
    assert!(store.get("buffer0").is_none());
    assert_eq!(store.get("named"), Some(b"manual".as_slice()));
}

#[test]
fn delete_without_name_rejects_named_only_store() {
    let mut store = BufferStore::new();
    store.set(Some("named"), b"manual".to_vec(), 50).unwrap();

    let err = store.delete(None).unwrap_err();
    assert!(err.to_string().contains("no buffer"));
    assert_eq!(store.get("named"), Some(b"manual".as_slice()));
}

#[test]
fn delete_named_buffer() {
    let mut store = BufferStore::new();
    store.set(Some("target"), b"data".to_vec(), 50).unwrap();
    let deleted = store.delete(Some("target")).unwrap();
    assert_eq!(deleted, "target");
    assert!(store.is_empty());
}

#[test]
fn delete_if_order_matches_removes_the_same_buffer_instance() {
    let mut store = BufferStore::new();
    store.set(Some("target"), b"data".to_vec(), 50).unwrap();
    let (_, _, order) = store.show_with_order(Some("target")).unwrap();

    assert!(store.delete_if_order_matches("target", order));
    assert!(store.get("target").is_none());
}

#[test]
fn delete_if_order_matches_skips_newer_replacements() {
    let mut store = BufferStore::new();
    store.set(Some("target"), b"old".to_vec(), 50).unwrap();
    let (_, _, order) = store.show_with_order(Some("target")).unwrap();
    store.set(Some("target"), b"new".to_vec(), 50).unwrap();

    assert!(!store.delete_if_order_matches("target", order));
    assert_eq!(store.get("target"), Some(b"new".as_slice()));
}

#[test]
fn delete_nonexistent_returns_error() {
    let mut store = BufferStore::new();
    let err = store.delete(Some("missing")).unwrap_err();
    assert!(err.to_string().contains("no buffer missing"));
}

#[test]
fn delete_empty_store_returns_error() {
    let mut store = BufferStore::new();
    let err = store.delete(None).unwrap_err();
    assert!(err.to_string().contains("no buffer"));
}

#[test]
fn show_returns_content() {
    let mut store = BufferStore::new();
    store.set(None, b"hello".to_vec(), 50).unwrap();
    let (name, content) = store.show(None).unwrap();
    assert_eq!(name, "buffer0");
    assert_eq!(content, b"hello");
}

#[test]
fn show_without_name_ignores_newer_named_buffers() {
    let mut store = BufferStore::new();
    store.set(None, b"auto".to_vec(), 50).unwrap();
    store.set(Some("named"), b"manual".to_vec(), 50).unwrap();

    let (name, content) = store.show(None).unwrap();
    assert_eq!(name, "buffer0");
    assert_eq!(content, b"auto");
}

#[test]
fn show_without_name_rejects_named_only_store() {
    let mut store = BufferStore::new();
    store.set(Some("named"), b"manual".to_vec(), 50).unwrap();

    let err = store.show(None).unwrap_err();
    assert!(err.to_string().contains("no buffers"));
    assert_eq!(store.get("named"), Some(b"manual".as_slice()));
}

#[test]
fn empty_content_does_not_create_a_buffer() {
    let mut store = BufferStore::new();
    let outcome = store.set(None, Vec::new(), 50).unwrap();

    assert_eq!(outcome.buffer_name(), None);
    assert!(store.show(None).is_err());
}

#[test]
fn show_nonexistent_returns_error() {
    let store = BufferStore::new();
    let err = store.show(Some("missing")).unwrap_err();
    assert!(err.to_string().contains("no buffer missing"));
}

#[test]
fn list_returns_formatted_entries_most_recent_first() {
    let mut store = BufferStore::new();
    store.set(None, b"first".to_vec(), 50).unwrap();
    store.set(None, b"second".to_vec(), 50).unwrap();
    let listing = store.list();
    assert_eq!(listing.len(), 2);
    assert!(listing[0].starts_with("buffer1:"));
    assert!(listing[1].starts_with("buffer0:"));
    assert!(listing[0].contains("6 bytes"));
    assert!(listing[1].contains("5 bytes"));
}

#[test]
fn empty_buffer_name_rejected() {
    let mut store = BufferStore::new();
    let err = store.set(Some(""), b"data".to_vec(), 50).unwrap_err();
    assert!(err.to_string().contains("empty"));
}

#[test]
fn buffer_names_may_contain_colons() {
    let mut store = BufferStore::new();
    store.set(Some("a:b"), b"data".to_vec(), 50).unwrap();
    assert_eq!(store.get("a:b"), Some(b"data".as_slice()));
}

#[test]
fn multiple_evictions_in_single_set() {
    let mut store = BufferStore::new();
    for _ in 0..5 {
        store.set(None, b"x".to_vec(), 5).unwrap();
    }
    assert_eq!(store.len(), 5);

    let outcome = store.set(None, b"new".to_vec(), 2).unwrap();
    assert_eq!(outcome.evicted().len(), 4);
    assert_eq!(store.len(), 2);
}

#[test]
fn buffer_preview_escapes_control_characters() {
    let content = b"hello\tworld\n\x00\x1b[31m";
    let preview = buffer_preview(content);
    assert!(preview.contains("\\t"));
    assert!(preview.contains("\\n"));
    assert!(preview.contains("\\000"));
    assert!(preview.contains("\\033"));
    assert!(!preview.contains('\n'));
    assert!(!preview.contains('\t'));
    assert!(!preview.contains('\0'));
}

#[test]
fn buffer_preview_truncates_long_content() {
    let content = "a".repeat(250);
    let preview = buffer_preview(content.as_bytes());
    assert_eq!(preview.len(), 203);
    assert!(preview.ends_with("..."));
}

#[test]
fn show_by_name_returns_correct_buffer() {
    let mut store = BufferStore::new();
    store.set(Some("alpha"), b"a-data".to_vec(), 50).unwrap();
    store.set(Some("beta"), b"b-data".to_vec(), 50).unwrap();
    let (name, content) = store.show(Some("alpha")).unwrap();
    assert_eq!(name, "alpha");
    assert_eq!(content, b"a-data");
}

#[test]
fn delete_by_name_leaves_other_buffers_intact() {
    let mut store = BufferStore::new();
    store.set(Some("keep"), b"keep".to_vec(), 50).unwrap();
    store.set(Some("remove"), b"rm".to_vec(), 50).unwrap();
    store.delete(Some("remove")).unwrap();
    assert_eq!(store.len(), 1);
    assert!(store.get("keep").is_some());
    assert!(store.get("remove").is_none());
}

#[test]
fn delete_stack_head_shifts_head_to_previous() {
    let mut store = BufferStore::new();
    store.set(None, b"a".to_vec(), 50).unwrap();
    store.set(None, b"b".to_vec(), 50).unwrap();
    store.set(None, b"c".to_vec(), 50).unwrap();
    assert_eq!(store.stack_head(), Some("buffer2"));
    store.delete(None).unwrap();
    assert_eq!(store.stack_head(), Some("buffer1"));
}

#[test]
fn rename_promotes_automatic_buffer_to_named_without_changing_order() {
    let mut store = BufferStore::new();
    store.set(None, b"data".to_vec(), 50).unwrap();

    let outcome = store.rename(Some("buffer0"), "renamed").unwrap();
    assert!(outcome.changed());
    assert_eq!(outcome.old_name(), "buffer0");
    assert_eq!(outcome.new_name(), "renamed");
    assert_eq!(store.stack_head(), Some("renamed"));
    assert_eq!(store.get("renamed"), Some(b"data".as_slice()));
}

#[test]
fn rename_replaces_existing_destination() {
    let mut store = BufferStore::new();
    store.set(Some("src"), b"src".to_vec(), 50).unwrap();
    store.set(Some("dst"), b"dst".to_vec(), 50).unwrap();

    let outcome = store.rename(Some("src"), "dst").unwrap();
    assert!(outcome.changed());
    assert!(outcome.replaced());
    assert!(store.get("src").is_none());
    assert_eq!(store.get("dst"), Some(b"src".as_slice()));
}

#[test]
fn rename_without_explicit_source_prefers_the_most_recent_unnamed_buffer() {
    let mut store = BufferStore::new();
    store.set(None, b"auto".to_vec(), 50).unwrap();
    store.set(Some("named"), b"manual".to_vec(), 50).unwrap();

    let outcome = store.rename(None, "renamed").unwrap();
    assert!(outcome.changed());
    assert_eq!(outcome.old_name(), "buffer0");
    assert_eq!(outcome.new_name(), "renamed");
    assert_eq!(store.get("renamed"), Some(b"auto".as_slice()));
    assert_eq!(store.get("named"), Some(b"manual".as_slice()));
}

#[test]
fn rename_without_explicit_source_rejects_named_only_store() {
    let mut store = BufferStore::new();
    store.set(Some("named"), b"manual".to_vec(), 50).unwrap();

    let error = store.rename(None, "renamed").unwrap_err();
    assert!(error.to_string().contains("no buffer"));
    assert_eq!(store.get("named"), Some(b"manual".as_slice()));
}
