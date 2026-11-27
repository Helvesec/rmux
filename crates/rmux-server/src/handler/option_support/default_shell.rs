use rmux_core::OptionStore;
use rmux_proto::{OptionName, RmuxError, SetOptionByNameRequest, SetOptionRequest};

pub(super) fn validate_typed_mutation(
    options: &OptionStore,
    request: &SetOptionRequest,
) -> Result<(), RmuxError> {
    if request.option != OptionName::DefaultShell {
        return Ok(());
    }

    let mut preview = options.clone();
    let outcome = preview.set(
        request.scope.clone(),
        request.option,
        request.value.clone(),
        request.mode,
    )?;
    validate_new_value(outcome.new_explicit.as_deref())
}

pub(super) fn validate_named_mutation(
    options: &OptionStore,
    request: &SetOptionByNameRequest,
    expanded_value: Option<&str>,
) -> Result<(), RmuxError> {
    if rmux_core::option_name_by_name(&request.name) != Some(OptionName::DefaultShell) {
        return Ok(());
    }

    let mut preview = options.clone();
    let outcome = preview.set_by_name(
        request.scope.clone(),
        &request.name,
        expanded_value.map(ToOwned::to_owned),
        request.mode,
        request.only_if_unset,
        request.unset,
        request.unset_pane_overrides,
    )?;
    validate_new_value(outcome.new_explicit.as_deref())
}

fn validate_new_value(value: Option<&str>) -> Result<(), RmuxError> {
    match value {
        Some(value) => validate_value(value),
        None => Ok(()),
    }
}

#[cfg(unix)]
fn validate_value(value: &str) -> Result<(), RmuxError> {
    if !value.is_empty() && !crate::terminal::is_suitable_shell(std::path::Path::new(value)) {
        return Err(RmuxError::Message(format!("not a suitable shell: {value}")));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_value(_value: &str) -> Result<(), RmuxError> {
    Ok(())
}
