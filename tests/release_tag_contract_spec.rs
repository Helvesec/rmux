#[test]
fn release_tag_surface_is_disarmed_and_create_only() {
    let policy: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/release-signers.json"))
            .expect("parse signer policy");
    assert_eq!(policy["schema_version"], 1);
    assert_eq!(policy["repository"]["id"], 1239918790);
    assert_eq!(policy["repository"]["full_name"], "Helvesec/rmux");
    assert_eq!(policy["release_app"]["app_id"], 4339867);
    assert_eq!(policy["release_app"]["may_create_only"], "refs/tags/v*");
    assert_eq!(policy["release_app"]["force_updates_allowed"], false);
    assert_eq!(policy["tag_policy"]["signature_format"], "ssh");
    assert_eq!(policy["tag_policy"]["signature_namespace"], "git");
    assert_eq!(
        policy["tag_policy"]["required_private_key_secret"],
        "RMUX_RELEASE_SSH_SIGNING_KEY"
    );
    assert_eq!(policy["tag_policy"]["enabled"], false);
    assert_eq!(
        policy["tag_policy"]["blocker"],
        "dedicated_release_ssh_signing_key_not_configured"
    );
    assert_eq!(
        policy["tag_policy"]["allowed_signers"]
            .as_array()
            .expect("signer array")
            .len(),
        0
    );

    let driver = include_str!("../scripts/release/sign-and-push-release-tag.sh");
    assert!(driver.contains("RMUX_RELEASE_APP_ID:-} == 4339867"));
    assert!(driver.contains("RMUX_RELEASE_APP_TOKEN"));
    assert!(driver.contains("push --porcelain release-origin"));
    assert!(driver.contains("refs/tags/$release_ref:refs/tags/$release_ref"));
    assert!(driver.contains("https://github.com/Helvesec/rmux.git"));
    assert!(driver.contains("GIT_ASKPASS=$askpass"));
    assert!(driver.contains("github_verification=$(verify_existing_ref \"$created_ref\")"));
    assert_eq!(driver.matches("verify_existing_ref").count(), 3);
    for forbidden in [
        "--force",
        "--force-with-lease",
        "--method POST",
        "--method PATCH",
        "--method DELETE",
        "gh release",
        "cargo publish",
    ] {
        assert!(
            !driver.contains(forbidden),
            "tag driver gained forbidden primitive {forbidden}"
        );
    }
}
