//! Regressions from the independent review of 5b8b45ac.
use super::kitty_runtime::{captured, input, output, setup};
use super::*;

pub(super) async fn bind(handler: &RequestHandler, key: &str) {
    bind_value(handler, "root", key, "BOUND").await;
}

async fn bind_value(handler: &RequestHandler, table: &str, key: &str, value: &str) {
    let response = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: table.into(),
            key: key.into(),
            note: None,
            repeat: false,
            command: Some(vec!["send-keys".into(), "-l".into(), value.into()]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
}

#[tokio::test]
async fn kitty_rereview_r2_primary_ctrl_shift_binding_precedes_shifted_and_any() {
    for table in ["root", "prefix"] {
        let (handler, _, pid, target) = setup(&format!("kitty-primary-{table}")).await;
        output(&handler, &target, b"\x1b[=31u").await;
        for (key, value) in [
            ("C-S-a", "PRIMARY"),
            ("C-A", "SHIFTED"),
            ("C-a", "PLAIN"),
            ("Any", "ANY"),
        ] {
            bind_value(&handler, table, key, value).await;
        }
        let mut expected = Vec::new();
        for identity in ["97", "97:65", "97:65:97"] {
            for text in ["", ";65"] {
                if table == "prefix" {
                    input(&handler, pid, b"\x02").await;
                }
                let frame =
                    format!("\x1b[{identity};6{text}u\x1b[{identity};6:2{text}u\x1b[97;6:3u");
                input(&handler, pid, frame.as_bytes()).await;
                expected.extend_from_slice(b"PRIMARY");
                captured(&handler, &target, &expected).await;
            }
        }
    }
}

#[tokio::test]
async fn kitty_rereview_r2_shifted_binding_falls_back_with_one_dispatch() {
    let (handler, _, pid, target) = setup("kitty-primary-fallback").await;
    output(&handler, &target, b"\x1b[=31u").await;
    for (key, value) in [
        ("C-+", "PLUS"),
        ("C-M-S-a", "PRIMARY"),
        ("C-M-A", "SHIFTED"),
        ("Any", "ANY"),
    ] {
        bind_value(&handler, "root", key, value).await;
    }
    input(&handler, pid, b"\x1b[61:43;6u\x1b[61:43;6:2u\x1b[61;6:3u").await;
    input(&handler, pid, b"\x1b[97:65;8u\x1b[97:65;8:2u\x1b[97;8:3u").await;
    captured(&handler, &target, b"PLUSPRIMARY").await;
    bind_value(&handler, "root", "C-S-=", "EQUALS").await;
    input(&handler, pid, b"\x1b[61:43;6u\x1b[61:43;6:2u\x1b[61;6:3u").await;
    captured(&handler, &target, b"PLUSPRIMARYEQUALS").await;
}

#[tokio::test]
async fn kitty_review_r2_shifted_bindings_consume_all_event_kinds() {
    let (handler, _, pid, target) = setup("kitty-review-shift-binding").await;
    output(&handler, &target, b"\x1b[=10u").await;
    for key in ["A", "M-A", "C-+", "É", "C-S-a"] {
        bind(&handler, key).await;
    }
    input(&handler, pid, b"A").await;
    for frame in [
        b"\x1b[97:65;2;65u\x1b[97:65;2:2;65u\x1b[97;2:3u".as_slice(),
        b"\x1b[97:65;4u\x1b[97:65;4:2u\x1b[97;4:3u",
        b"\x1b[61:43;6u\x1b[61:43;6:2u\x1b[61;6:3u",
        b"\x1b[233:201;2u\x1b[233:201;2:2u\x1b[233;2:3u",
        b"\x1b[97;6u\x1b[97;6:2u\x1b[97;6:3u",
    ] {
        input(&handler, pid, frame).await;
    }
    captured(&handler, &target, b"BOUNDBOUNDBOUNDBOUNDBOUNDBOUND").await;
}

#[tokio::test]
async fn kitty_review_r2_shift_projection_keeps_protocol_identity_and_fallback_modes() {
    let (handler, _, pid, target) = setup("kitty-review-shift-fallback").await;
    handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExtendedKeys,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    output(&handler, &target, b"\x1b[=31u").await;
    input(
        &handler,
        pid,
        b"\x1b[97:65;2;65u\x1b[97:65;2:2;65u\x1b[97;2:3u",
    )
    .await;
    output(&handler, &target, b"\x1b[<99u").await;
    input(
        &handler,
        pid,
        b"\x1b[97:65;2u\x1b[97:65;2;65u\x1b[233:201;2u\x1b[97:65;4u\x1b[97;4u\x1b[13;2u\x1b[9;2u",
    )
    .await;
    output(&handler, &target, b"\x1b[>4;2m").await;
    input(&handler, pid, b"\x1b[13;2u").await;
    captured(
        &handler,
        &target,
        "\x1b[97:65;2;65u\x1b[97:65;2:2;65u\x1b[97;2:3uAAÉ\x1bA\x1bA\n\x1b[Z\x1b[27;2;13~"
            .as_bytes(),
    )
    .await;
}
