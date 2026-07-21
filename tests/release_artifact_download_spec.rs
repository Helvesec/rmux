const SOURCES: &[(&str, &str)] = &[
    (
        ".github/actions/canonical-smoke/action.yml",
        include_str!("../.github/actions/canonical-smoke/action.yml"),
    ),
    (
        ".github/actions/release-policy-audit/action.yml",
        include_str!("../.github/actions/release-policy-audit/action.yml"),
    ),
    (
        ".github/actions/release-publication-inputs/action.yml",
        include_str!("../.github/actions/release-publication-inputs/action.yml"),
    ),
    (
        ".github/workflows/release-chocolatey-retry.yml",
        include_str!("../.github/workflows/release-chocolatey-retry.yml"),
    ),
    (
        ".github/workflows/release-downstream.yml",
        include_str!("../.github/workflows/release-downstream.yml"),
    ),
    (
        ".github/workflows/release-policy-audit.yml",
        include_str!("../.github/workflows/release-policy-audit.yml"),
    ),
    (
        ".github/workflows/release-promote.yml",
        include_str!("../.github/workflows/release-promote.yml"),
    ),
    (
        ".github/workflows/release-promotion-simulation.yml",
        include_str!("../.github/workflows/release-promotion-simulation.yml"),
    ),
    (
        ".github/workflows/release-receipt.yml",
        include_str!("../.github/workflows/release-receipt.yml"),
    ),
    (
        ".github/workflows/release-shadow.yml",
        include_str!("../.github/workflows/release-shadow.yml"),
    ),
    (
        ".github/workflows/release-snap-retry.yml",
        include_str!("../.github/workflows/release-snap-retry.yml"),
    ),
    (
        ".github/workflows/release-tag-authoring.yml",
        include_str!("../.github/workflows/release-tag-authoring.yml"),
    ),
];

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[test]
fn exact_single_artifact_downloads_extract_into_the_requested_directory() {
    let mut single_downloads = 0;
    let mut multi_downloads = 0;

    for (path, source) in SOURCES {
        let lines: Vec<_> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let Some(value) = line.trim().strip_prefix("artifact-ids:") else {
                continue;
            };
            let value = value.trim();
            let key_indent = indentation(line);
            let merge = lines[index + 1..]
                .iter()
                .take_while(|next| next.trim().is_empty() || indentation(next) >= key_indent)
                .find_map(|next| next.trim().strip_prefix("merge-multiple:").map(str::trim));

            if value == "${{ steps.artifacts.outputs.artifact_ids }}" {
                multi_downloads += 1;
                assert_ne!(
                    merge,
                    Some("true"),
                    "{path}:{index} flattens eleven artifacts"
                );
            } else {
                single_downloads += 1;
                assert_eq!(
                    merge,
                    Some("true"),
                    "{path}:{index} leaves one artifact in an unexpected name directory"
                );
            }
        }
    }

    assert_eq!(single_downloads, 29);
    assert_eq!(multi_downloads, 4);
}
