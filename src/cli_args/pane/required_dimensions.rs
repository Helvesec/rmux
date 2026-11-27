pub(super) fn validate_required_absolute_resize_arguments(
    arguments: &[String],
) -> Result<(), clap::Error> {
    let spec = rmux_core::command_inventory::command_short_option_spec("resize-pane")
        .expect("resize-pane command schema");
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let Some(flags) = argument
            .strip_prefix('-')
            .filter(|flags| !flags.is_empty() && !flags.starts_with('-'))
        else {
            break;
        };
        let mut flags = flags.chars().peekable();
        while let Some(flag) = flags.next() {
            if spec.takes_value(flag) {
                // A required value belongs to this option even when it is
                // another flag or `--`. Attached values end the cluster.
                let missing = flags.peek().is_none() && arguments.next().is_none();
                if missing && matches!(flag, 'x' | 'y') {
                    return Err(clap::Error::raw(
                        clap::error::ErrorKind::ValueValidation,
                        format!("command resize-pane: -{flag} expects an argument"),
                    ));
                }
                break;
            }
            if !spec.is_boolean(flag) {
                break;
            }
        }
    }
    Ok(())
}
