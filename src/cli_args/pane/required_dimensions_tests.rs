use super::validate_required_absolute_resize_arguments;

#[test]
fn required_dimensions_only_recognize_real_options() {
    for arguments in [
        vec!["-x", "20", "-t", "-y"],
        vec!["-y", "20", "-t", "-x"],
        vec!["-x20", "-t-y"],
        vec!["-Zt", "-x"],
        vec!["-x", "-y"],
        vec!["-y", "-x"],
        vec!["-X", "-y"],
        vec!["-Y", "-x"],
        vec!["--", "-x"],
        vec!["3", "-y"],
        vec!["-", "-x"],
        vec!["-R", "3", "-y"],
    ] {
        let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
        validate_required_absolute_resize_arguments(&arguments)
            .unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
    }
}

#[test]
fn required_dimensions_report_missing_separated_and_clustered_values() {
    for (arguments, flag) in [
        (vec!["-x"], "-x"),
        (vec!["-y"], "-y"),
        (vec!["-Zx"], "-x"),
        (vec!["-Ty"], "-y"),
        (vec!["-t", "--", "-x"], "-x"),
        (vec!["-t-y", "-y"], "-y"),
        (vec!["-x20", "-y"], "-y"),
    ] {
        let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let error = validate_required_absolute_resize_arguments(&arguments).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("{flag} expects an argument")),
            "{arguments:?}: {error}"
        );
    }
}
