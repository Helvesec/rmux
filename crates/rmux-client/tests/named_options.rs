#![cfg(unix)]

mod common;

use rmux_client::connect;
use rmux_proto::{
    OptionName, OptionScopeSelector, Request, Response, ScopeSelector, SetOptionByNameRequest,
    SetOptionMode,
};

#[test]
fn named_builtins_and_typed_options_share_a_connection_after_application_rejection(
) -> Result<(), Box<dyn std::error::Error>> {
    let harness = common::TestHarness::new("named-options");
    let mut server = common::start_server(&harness)?;
    let mut client = connect(harness.socket_path())?;
    let name = "window-pane-status-format";
    assert!(matches!(
        client.set_option_by_name(
            OptionScopeSelector::WindowGlobal,
            name.into(),
            Some("named".into()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )?,
        Response::SetOptionByName(_)
    ));
    let before =
        client.show_options(OptionScopeSelector::WindowGlobal, None, false, false, false)?;
    for unset in [false, true] {
        let indexed = format!("{name}[0]");
        assert!(client
            .set_option_by_name(
                OptionScopeSelector::WindowGlobal,
                indexed.clone(),
                (!unset).then(|| "INDEX".into()),
                SetOptionMode::Replace,
                false,
                unset,
                false,
            )
            .is_err());
        let response = client.roundtrip(&Request::SetOptionByName(Box::new(
            SetOptionByNameRequest {
                scope: OptionScopeSelector::WindowGlobal,
                name: indexed.clone(),
                value: (!unset).then(|| "INDEX".into()),
                mode: SetOptionMode::Replace,
                only_if_unset: false,
                unset,
                unset_pane_overrides: false,
                format: false,
                format_target: None,
            },
        )))?;
        assert!(matches!(response, Response::Error(error)
            if error.error == rmux_proto::RmuxError::InvalidSetOption(format!("not an array: {indexed}"))));
        assert_eq!(
            client.show_options(OptionScopeSelector::WindowGlobal, None, false, false, false)?,
            before
        );
    }
    assert!(matches!(
        client.roundtrip(&Request::SetOptionByName(Box::new(
            SetOptionByNameRequest {
                scope: OptionScopeSelector::WindowGlobal,
                name: "absent-frozen-option".into(),
                value: Some("bad".into()),
                mode: SetOptionMode::Replace,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                format: false,
                format_target: None,
            }
        )))?,
        Response::Error(_)
    ));
    assert_eq!(
        client.show_options(OptionScopeSelector::WindowGlobal, None, false, false, false)?,
        before
    );
    assert!(matches!(
        client.set_option(
            ScopeSelector::Global,
            OptionName::Status,
            "off".into(),
            SetOptionMode::Replace
        )?,
        Response::SetOption(_)
    ));
    let Response::ShowOptions(shown) = client.show_options(
        OptionScopeSelector::WindowGlobal,
        Some(name.into()),
        true,
        false,
        false,
    )?
    else {
        panic!("expected show response")
    };
    assert_eq!(shown.output.stdout, b"named\n");
    server.shutdown()?;
    Ok(())
}
