#[path = "support/python3.rs"]
mod python3;

use std::path::PathBuf;
use std::process::Output;

const CHOCOLATEY: &str = include_str!("../.github/workflows/release-chocolatey-retry.yml");
const PREPARE_ACTION: &str =
    include_str!("../.github/actions/release-channel-retry-prepare/action.yml");
const SNAP: &str = include_str!("../.github/workflows/release-snap-retry.yml");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn assert_python(source: &str) {
    let output = python3::command()
        .args(["-c", source])
        .current_dir(repo_root())
        .output()
        .expect("run release retry input fixture");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repeated(character: char, count: usize) -> String {
    std::iter::repeat_n(character, count).collect()
}

fn identity_args(channel: &str) -> Vec<(String, String)> {
    vec![
        ("channel".into(), channel.into()),
        ("source-sha".into(), repeated('a', 40)),
        ("release-id".into(), "7".into()),
        ("release-ref".into(), "v1.2.3-rc.4".into()),
        (
            "idempotency-key".into(),
            format!("rmux-downstream-v1:{}", repeated('b', 64)),
        ),
    ]
}

fn prepare_args() -> Vec<(String, String)> {
    let mut args = identity_args("chocolatey");
    args.extend([
        ("receipt-run-id".into(), "101".into()),
        ("receipt-run-workflow-id".into(), "202".into()),
        ("receipt-workflow-id".into(), "202".into()),
        ("receipt-artifact-id".into(), "301".into()),
        (
            "receipt-artifact-digest".into(),
            format!("sha256:{}", repeated('c', 64)),
        ),
        ("receipt-envelope-artifact-id".into(), "302".into()),
        (
            "receipt-envelope-artifact-digest".into(),
            format!("sha256:{}", repeated('d', 64)),
        ),
        ("receipt-predicate-sha256".into(), repeated('e', 64)),
        ("receipt-envelope-sha256".into(), repeated('f', 64)),
        ("prior-result-run-id".into(), "101".into()),
        ("prior-result-run-workflow-id".into(), "202".into()),
        ("prior-result-producer-workflow-id".into(), "202".into()),
        (
            "prior-result-producer-workflow-path".into(),
            ".github/workflows/release-receipt.yml".into(),
        ),
        ("prior-result-artifact-id".into(), "401".into()),
        (
            "prior-result-artifact-digest".into(),
            format!("sha256:{}", repeated('0', 64)),
        ),
        ("prior-result-predicate-sha256".into(), repeated('1', 64)),
        ("prior-result-envelope-artifact-id".into(), "402".into()),
        (
            "prior-result-envelope-artifact-digest".into(),
            format!("sha256:{}", repeated('2', 64)),
        ),
        ("prior-result-envelope-sha256".into(), repeated('3', 64)),
    ]);
    args
}

fn run_validator(command: &str, args: &[(String, String)]) -> Output {
    let mut process = python3::command();
    process
        .arg("scripts/release/prepare-channel-retry.py")
        .arg(command)
        .current_dir(repo_root());
    for (name, value) in args {
        process.arg(format!("--{name}")).arg(value);
    }
    process.output().expect("run retry input validator")
}

fn replace_arg(
    args: &[(String, String)],
    target: &str,
    replacement: &str,
) -> Vec<(String, String)> {
    args.iter()
        .map(|(name, value)| {
            (
                name.clone(),
                if name == target {
                    replacement.to_owned()
                } else {
                    value.clone()
                },
            )
        })
        .collect()
}

fn assert_rejected(command: &str, args: &[(String, String)], label: &str) {
    let output = run_validator(command, args);
    assert!(
        !output.status.success(),
        "{label} was accepted\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_environment_identity_validator(idempotency_key: &str, omitted: Option<&str>) -> Output {
    let mut process = python3::command();
    process
        .args([
            "scripts/release/prepare-channel-retry.py",
            "validate-identity-inputs",
            "--from-env",
        ])
        .current_dir(repo_root());
    for (name, value) in [
        ("RMUX_CHANNEL", "chocolatey"),
        (
            "RMUX_SOURCE_SHA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        ("RMUX_RELEASE_ID", "17"),
        ("RMUX_RELEASE_REF", "v1.2.3"),
        ("RMUX_REQUEST_IDEMPOTENCY_KEY", idempotency_key),
    ] {
        if omitted != Some(name) {
            process.env(name, value);
        } else {
            process.env_remove(name);
        }
    }
    process
        .output()
        .expect("run retry environment input validator")
}

#[test]
fn retry_workflows_never_embed_dispatch_inputs_in_run() {
    assert_python(
        r#"
import pathlib
import sys

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import (
    find_direct_input_expressions,
    validate_no_direct_input_expressions,
)

invalid = (
    """
steps:
  - name: structural marker
    run: |
      echo "${{ inputs.harmless_marker }}"
""",
    """
steps:
  - {name: structural marker, run: "echo ${{github.event.inputs.harmless_marker}}"}
""",
    """
steps:
  - run: echo "${{ format('{0}', inputs.harmless_marker) }}"
""",
)
for fixture in invalid:
    if len(find_direct_input_expressions(fixture)) != 1:
        raise SystemExit("direct input fixture was not detected")

safe = """
jobs:
  marker:
    name: ${{ inputs.harmless_marker }}
    if: inputs.harmless_marker != ''
    steps:
      - env:
          RMUX_MARKER: ${{ inputs.harmless_marker }}
        run: echo "$RMUX_MARKER"
      - uses: example/action@0000000000000000000000000000000000000000
        with:
          marker: ${{ inputs.harmless_marker }}
      - run: echo "${{ steps.marker.outputs.value }}"
"""
if find_direct_input_expressions(safe):
    raise SystemExit("Actions-engine fixture was mistaken for shell interpolation")

root = pathlib.Path.cwd()
validate_no_direct_input_expressions(
    (
        root / ".github/workflows/release-channel-retry.yml",
        root / ".github/workflows/release-chocolatey-retry.yml",
        root / ".github/workflows/release-snap-retry.yml",
        root / ".github/actions/release-channel-retry-prepare/action.yml",
    )
)
"#,
    );
}

#[test]
fn retry_boundary_scanner_covers_twelve_yaml_scalar_and_context_forms() {
    assert_python(
        r###"
import pathlib
import sys

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import find_direct_input_expressions

safe = "${{ steps.marker.outputs.value }}"
active = "${{ inputs.marker }}"
active_bare_context = "${{ format('{0}', inputs) }}"
forms = (
    ("plain", """steps:
  - run: echo __EXPR__
""", active),
    ("single_quoted", """steps:
  - run: 'echo __EXPR__'
""", active),
    ("double_quoted", """steps:
  - run: "echo __EXPR__"
""", active),
    ("literal", """steps:
  - run: |-
      echo __EXPR__
""", active),
    ("folded", """steps:
  - run: >+
      echo __EXPR__
""", active),
    ("flow_quoted", """steps:
  - {name: flow quoted, run: "echo __EXPR__"}
""", active),
    ("flow_plain", """steps:
  - [{name: flow plain, run: echo-__EXPR__}]
""", active),
    ("multiline_plain", """steps:
  - run: echo first
      __EXPR__
""", active),
    ("multiline_single", """steps:
  - run: 'echo first
      __EXPR__'
""", active),
    ("multiline_double", """steps:
  - run: "echo first
      __EXPR__"
""", active),
    ("quoted_flow_key", """steps:
  - {"run": "echo __EXPR__", name: quoted key}
""", active),
    ("bare_inputs_context", """steps:
  - run: "echo __EXPR__"
""", active_bare_context),
)

missed = []
false_positives = []
for name, template, mutant in forms:
    inert = template.replace("__EXPR__", safe)
    if find_direct_input_expressions(inert):
        false_positives.append(name)
    active_fixture = template.replace("__EXPR__", mutant)
    if len(find_direct_input_expressions(active_fixture)) != 1:
        missed.append(name)

if false_positives:
    raise SystemExit(f"safe YAML forms were rejected: {false_positives}")
if missed:
    raise SystemExit(f"active YAML forms were missed: {missed}")

root = pathlib.Path.cwd()
for relative in (
    ".github/workflows/release-chocolatey-retry.yml",
    ".github/workflows/release-snap-retry.yml",
):
    findings = find_direct_input_expressions(
        (root / relative).read_text(encoding="utf-8")
    )
    if findings:
        raise SystemExit(f"clean workflow rejected: {relative}: {findings}")
"###,
    );
}

#[test]
fn retry_boundary_detects_github_all_bracket_access() {
    assert_python(
        r###"
import sys

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import find_direct_input_expressions

fixture = """steps:
  - run: echo "${{ github['event']['inputs']['all_brackets'] }}"
"""
contexts = tuple(
    finding.context for finding in find_direct_input_expressions(fixture)
)
if contexts != ("github.event.inputs",):
    raise SystemExit(f"all-bracket access was missed: {contexts}")
"###,
    );
}

#[test]
fn retry_boundary_detects_github_inputs_bracket_access() {
    assert_python(
        r###"
import sys

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import find_direct_input_expressions

fixture = """steps:
  - run: echo "${{ github.event['inputs']['input_bracket'] }}"
"""
contexts = tuple(
    finding.context for finding in find_direct_input_expressions(fixture)
)
if contexts != ("github.event.inputs",):
    raise SystemExit(f"bracketed inputs access was missed: {contexts}")
"###,
    );
}

#[test]
fn retry_boundary_detects_github_event_bracket_access() {
    assert_python(
        r###"
import sys

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import find_direct_input_expressions

fixture = """steps:
  - run: echo "${{ github['event'].inputs.event_bracket }}"
"""
contexts = tuple(
    finding.context for finding in find_direct_input_expressions(fixture)
)
if contexts != ("github.event.inputs",):
    raise SystemExit(f"bracketed event access was missed: {contexts}")
"###,
    );
}

#[test]
fn retry_boundary_ignores_terminator_inside_expression_string() {
    assert_python(
        r###"
import sys

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import find_direct_input_expressions

fixture = """steps:
  - run: echo "${{ format('{{safe}} {0}', inputs.after_terminator) }}"
"""
contexts = tuple(
    finding.context for finding in find_direct_input_expressions(fixture)
)
if contexts != ("inputs",):
    raise SystemExit(f"post-string input access was missed: {contexts}")
"###,
    );
}

#[test]
fn retry_public_validator_preserves_r4_yaml_expression_boundaries() {
    assert_python(
        r###"
import pathlib
import sys
import tempfile

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import validate_no_direct_input_expressions

governed = (
    (
        "quoted_terminator",
        """steps:
  - run: echo "${{ format('display }} only: {0}', inputs.release_ref) }}"
""",
        True,
    ),
    (
        "quoted_both_markers",
        """steps:
  - run: echo "${{ format('text ${{ and }} only', github['event'].inputs.channel) }}"
""",
        True,
    ),
    (
        "indexed_event_dotted_inputs",
        """steps:
  - run: echo "${{ github['event'].inputs.release_id }}"
""",
        True,
    ),
    (
        "dotted_event_indexed_inputs",
        """steps:
  - run: "echo ${{ github.event['inputs'].release_id }}"
""",
        True,
    ),
    (
        "all_indexed",
        """steps:
  - run: |-
      echo "${{ github['event']['inputs']['release_id'] }}"
""",
        True,
    ),
    (
        "spaced_mixed",
        """steps:
  - run: >+
      echo "${{ github [ 'event' ] . inputs [ 'release_id' ] }}"
""",
        True,
    ),
)

protected = (
    (
        "block_single_active",
        """steps:
  - run: 'Write-Output "${{ github[''event''].inputs.release_id }}"'
""",
        True,
    ),
    (
        "flow_single_active",
        """steps:
  - {name: emit, run: 'printf "%s" "${{ github[''event''].inputs.artifact_id }}"', shell: bash}
""",
        True,
    ),
    (
        "block_single_inert",
        """steps:
  - run: 'echo "${{ ''inputs.release_ref is documentation'' }}"'
""",
        False,
    ),
    (
        "flow_single_inert",
        """steps:
  - {shell: bash, run: 'echo "${{ ''github.event.inputs.not_an_access'' }}"'}
""",
        False,
    ),
    (
        "unclosed_block_single",
        """steps:
  - run: 'echo bounded
    env:
      DOCUMENT_ONLY: ${{ inputs.outside_run }}
""",
        False,
    ),
    (
        "unclosed_block_double",
        """steps:
  - run: "Write-Output bounded
    name: ${{ github.event.inputs.outside_run }}
""",
        False,
    ),
    (
        "unclosed_flow_single",
        """steps:
  - {run: 'echo bounded, name: ${{ inputs.outside_run }} }
""",
        False,
    ),
)

failures = []
passed = {"governed": 0, "protected": 0}
with tempfile.TemporaryDirectory(
    prefix=".rmux-r4-boundary-",
    dir=pathlib.Path.cwd(),
) as temporary:
    fixture = pathlib.Path(temporary) / "workflow.yml"
    for group, cases in (("governed", governed), ("protected", protected)):
        for name, source, expected_rejection in cases:
            fixture.write_bytes(source.encode("utf-8"))
            try:
                validate_no_direct_input_expressions((fixture,))
            except ValueError:
                actual_rejection = True
            else:
                actual_rejection = False
            if actual_rejection == expected_rejection:
                passed[group] += 1
            else:
                failures.append(
                    f"{group}/{name}: expected rejection={expected_rejection}, "
                    f"actual={actual_rejection}"
                )

if failures:
    raise SystemExit(
        f"governed={passed['governed']}/{len(governed)}, "
        f"protected={passed['protected']}/{len(protected)}: "
        + " | ".join(failures)
    )
"###,
    );
}

#[test]
fn retry_public_validator_bounds_quotes_without_narrowing_multiline_yaml() {
    assert_python(
        r###"
import pathlib
import sys
import tempfile

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import validate_no_direct_input_expressions

cases = (
    (
        "minimally_indented_multiline_single",
        """steps:
  - run: 'echo first
    ${{ inputs.multiline_single }}'
""",
        True,
    ),
    (
        "minimally_indented_multiline_double",
        """steps:
  - run: "echo first
    ${{ github.event.inputs.multiline_double }}"
""",
        True,
    ),
    (
        "unclosed_flow_single_cannot_borrow_next_step_quote",
        """steps:
  - {run: 'echo bounded, name: ${{ inputs.outside_run }} }
  - name: 'later quote'
    run: echo safe
""",
        False,
    ),
    (
        "unclosed_flow_double_cannot_borrow_next_step_quote",
        """steps:
  - {run: "echo bounded, name: ${{ github.event.inputs.outside_run }} }
  - name: "later quote"
    run: echo safe
""",
        False,
    ),
)

failures = []
with tempfile.TemporaryDirectory(
    prefix=".rmux-r4-quote-boundary-",
    dir=pathlib.Path.cwd(),
) as temporary:
    fixture = pathlib.Path(temporary) / "workflow.yml"
    for name, source, expected_rejection in cases:
        fixture.write_bytes(source.encode("utf-8"))
        try:
            validate_no_direct_input_expressions((fixture,))
        except ValueError:
            actual_rejection = True
        else:
            actual_rejection = False
        if actual_rejection != expected_rejection:
            failures.append(
                f"{name}: expected rejection={expected_rejection}, "
                f"actual={actual_rejection}"
            )

if failures:
    raise SystemExit("quote-boundary regressions: " + " | ".join(failures))
"###,
    );
}

#[test]
fn retry_boundary_scanner_covers_valid_expression_neighborhood() {
    assert_python(
        r###"
import sys

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import find_direct_input_expressions

cases = (
    (
        "inputs_dotted_plain",
        """steps:
  - run: echo "${{ inputs.pointed }}"
""",
        ("inputs",),
    ),
    (
        "inputs_indexed_single_quoted_yaml",
        """steps:
  - run: 'echo ${{ inputs[''indexed''] }}'
""",
        ("inputs",),
    ),
    (
        "github_dotted_double_quoted_yaml",
        """steps:
  - run: "echo ${{ github.event.inputs.pointed }}"
""",
        ("github.event.inputs",),
    ),
    (
        "github_all_indexed_literal",
        """steps:
  - run: |
      echo "${{ github['event']['inputs']['indexed'] }}"
""",
        ("github.event.inputs",),
    ),
    (
        "github_mixed_folded",
        """steps:
  - run: >-
      echo "${{ github['event'].inputs.mixed }}"
""",
        ("github.event.inputs",),
    ),
    (
        "github_spaced_mixed",
        """steps:
  - run: echo "${{ github [ 'event' ] . inputs [ 'spaced' ] }}"
""",
        ("github.event.inputs",),
    ),
    (
        "escaped_single_quote_and_markers",
        """steps:
  - run: |
      echo "${{ format('it''s }} and ${{ inert', inputs.after_escape) }}"
""",
        ("inputs",),
    ),
    (
        "nested_functions_arrays_and_object_string",
        """steps:
  - run: |
      echo "${{ contains(fromJSON('{"values":["}}","${{"]}').values, inputs.nested) }}"
""",
        ("inputs",),
    ),
    (
        "flow_plain_expression_string_terminator",
        """steps:
  - {run: echo-${{ format('{{safe}} {0}', inputs.flow_plain) }}, shell: bash}
""",
        ("inputs",),
    ),
    (
        "multiple_expressions",
        """steps:
  - run: echo "${{ steps.safe.outputs.value }} ${{ inputs.first }} ${{ github['event'].inputs.second }}"
""",
        ("inputs", "github.event.inputs"),
    ),
    (
        "shell_comment_is_still_active",
        """steps:
  - run: |
      # ${{ github.event['inputs'].commented }}
      echo safe
""",
        ("github.event.inputs",),
    ),
    (
        "bare_inputs_function_argument",
        """steps:
  - run: echo "${{ toJSON(inputs) }}"
""",
        ("inputs",),
    ),
)

errors = []
for name, fixture, expected in cases:
    actual = tuple(
        finding.context for finding in find_direct_input_expressions(fixture)
    )
    if actual != expected:
        errors.append(f"{name}: expected={expected}, actual={actual}")
if errors:
    raise SystemExit("valid expression neighborhood failed: " + " | ".join(errors))
"###,
    );
}

#[test]
fn retry_boundary_scanner_ignores_inert_and_malformed_neighborhood() {
    assert_python(
        r###"
import sys

sys.path.insert(0, "scripts/release")
from workflow_run_input_boundary import find_direct_input_expressions

cases = (
    (
        "ordinary_shell_literal",
        """steps:
  - run: echo 'inputs.literal github.event.inputs.literal'
""",
    ),
    (
        "single_quoted_expression_literal",
        """steps:
  - run: echo "${{ 'inputs.literal' }}"
""",
    ),
    (
        "escaped_expression_string_with_markers",
        """steps:
  - run: |
      echo "${{ 'it''s inputs.literal }} and ${{ literal' }}"
""",
    ),
    (
        "double_quoted_invalid_expression_string",
        """steps:
  - run: echo '${{ "inputs.invalid" }}'
""",
    ),
    (
        "unbalanced_parenthesis",
        """steps:
  - run: echo "${{ contains(inputs.invalid, 'x' }}"
""",
    ),
    (
        "unterminated_single_quote",
        """steps:
  - run: echo "${{ inputs.invalid && 'unterminated }}"
""",
    ),
    (
        "neighbor_identifiers",
        """steps:
  - run: echo "${{ inputs_suffix || github.event.inputs_suffix }}"
""",
    ),
    (
        "non_run_yaml_scalars",
        """name: ${{ inputs.workflow_name }}
env:
  ORDINARY: ${{ github.event.inputs.environment_value }}
steps:
  - name: ${{ inputs.step_name }}
    run: echo safe
""",
    ),
    (
        "yaml_comments",
        """# run: echo "${{ inputs.top_comment }}"
steps:
  # - run: echo "${{ github.event.inputs.step_comment }}"
  - run: echo safe # inputs.comment_text
""",
    ),
)

errors = []
for name, fixture in cases:
    findings = find_direct_input_expressions(fixture)
    if findings:
        errors.append(f"{name}: {findings}")
if errors:
    raise SystemExit("inert or malformed YAML was rejected: " + " | ".join(errors))
"###,
    );
}

#[test]
fn retry_validators_read_untrusted_values_from_fixed_environment_names() {
    let canonical_key = format!("rmux-downstream-v1:{}", repeated('b', 64));
    let output = run_environment_identity_validator(&canonical_key, None);
    assert!(
        output.status.success(),
        "canonical environment validation failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for (value, label) in [
        (
            format!(
                "rmux-downstream-v1:{}\"{}",
                repeated('b', 32),
                repeated('b', 32)
            ),
            "embedded double quote",
        ),
        (String::new(), "explicit empty value"),
    ] {
        let output = run_environment_identity_validator(&value, None);
        assert!(
            !output.status.success(),
            "{label} was accepted after direct environment transport"
        );
    }
    let output = run_environment_identity_validator(&canonical_key, Some("RMUX_RELEASE_ID"));
    assert!(
        !output.status.success(),
        "missing environment value was mistaken for an explicit value"
    );

    assert!(
        PREPARE_ACTION.contains("validate-prepare-inputs --from-env"),
        "prepare validator must read the fixed environment projection"
    );
    assert!(
        PREPARE_ACTION.contains("prepare --from-env"),
        "prepare must revalidate the fixed environment projection before I/O"
    );
    for (name, workflow) in [("Chocolatey", CHOCOLATEY), ("Snap", SNAP)] {
        assert!(
            workflow.contains("validate-identity-inputs --from-env"),
            "{name} identity validator still receives raw argv values"
        );
        assert!(
            workflow.contains("verify-prepared --from-env"),
            "{name} prepared verifier still receives raw argv values"
        );
    }
    for (source, forbidden) in [
        (
            CHOCOLATEY,
            "validate-identity-inputs `\n            --channel",
        ),
        (CHOCOLATEY, "verify-prepared `\n            --prepared"),
        (SNAP, "validate-identity-inputs \\\n            --channel"),
        (SNAP, "verify-prepared \\\n            --prepared"),
        (
            PREPARE_ACTION,
            "validate-prepare-inputs \\\n          --channel",
        ),
        (PREPARE_ACTION, "prepare \\\n          --root"),
    ] {
        assert!(
            !source.contains(forbidden),
            "untrusted retry value remains on a shell argv boundary: {forbidden}"
        );
    }
}

#[test]
fn retry_input_validators_accept_canonical_and_reject_invalid_forms() {
    let identity = identity_args("snap_candidate");
    let output = run_validator("validate-identity-inputs", &identity);
    assert!(
        output.status.success(),
        "canonical identity failed\nstderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let prepare = prepare_args();
    let output = run_validator("validate-prepare-inputs", &prepare);
    assert!(
        output.status.success(),
        "canonical prepare inputs failed\nstderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    for (field, value, label) in [
        ("channel", "harmless_marker", "unknown channel"),
        (
            "source-sha",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "uppercase source SHA",
        ),
        ("release-id", "01", "noncanonical release ID"),
        ("release-ref", "v1.2", "incomplete release ref"),
        (
            "idempotency-key",
            "rmux-downstream-v1:harmless-marker",
            "malformed idempotency key",
        ),
    ] {
        assert_rejected(
            "validate-identity-inputs",
            &replace_arg(&identity, field, value),
            label,
        );
    }

    for (field, value, label) in [
        ("receipt-artifact-id", "0", "zero artifact ID"),
        (
            "receipt-artifact-digest",
            "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "uppercase artifact digest",
        ),
        (
            "receipt-predicate-sha256",
            "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
            "nonhex predicate digest",
        ),
        (
            "prior-result-producer-workflow-path",
            ".github/workflows/release-policy-channel.yml",
            "cross-channel producer path",
        ),
        ("prior-result-run-id", "102", "cross-run mismatch"),
        (
            "prior-result-run-workflow-id",
            "203",
            "workflow identity mismatch",
        ),
    ] {
        assert_rejected(
            "validate-prepare-inputs",
            &replace_arg(&prepare, field, value),
            label,
        );
    }
}
