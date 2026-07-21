use std::fs;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn policy_audit_simulation_is_nonpublishing_with_two_exact_callers() {
    let workflow = include_str!("../.github/workflows/release-policy-audit.yml");
    let action = include_str!("../.github/actions/release-policy-audit/action.yml");
    let activation: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/release-activation.json"))
            .expect("parse activation ledger");
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/policy-audit-contract.json"
    ))
    .expect("parse policy audit contract");
    let reference_schema: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/schemas/policy-audit-reference.schema.json"
    ))
    .expect("parse policy audit reference schema");

    assert!(workflow.contains("on:\n  workflow_call:"));
    assert_eq!(workflow.matches("if: ${{ inputs.simulation }}").count(), 1);
    assert_eq!(workflow.matches("permissions: {}").count(), 1);
    assert!(workflow.contains("assert-release-capability.py policy_audit"));
    assert!(!workflow.contains("environment: release-policy-audit"));
    assert!(!workflow.contains("RMUX_POLICY_AUDIT_APP_PRIVATE_KEY"));
    assert!(!workflow.contains("actions/create-github-app-token@"));
    assert!(
        action.contains("actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349")
    );
    assert!(workflow.contains("scripts/release/policy-root.py"));
    assert!(workflow.contains("--expected-workflow-id 316223904"));
    assert!(workflow.contains("candidate manifest SHA-256 differs"));
    for permission in [
        "permission-actions: read",
        "permission-administration: write",
        "permission-metadata: read",
    ] {
        assert!(action.contains(permission), "missing {permission}");
    }
    assert!(action.contains("--audit-workflow-id"));
    assert!(action.contains("--audit-workflow-path"));
    let guard = workflow
        .find("assert-release-capability.py policy_audit")
        .expect("capability guard");
    let bundle = workflow
        .find("Upload the unprivileged exact policy bundle")
        .expect("policy bundle upload");
    assert!(
        guard < bundle,
        "capability guard must run before bundle upload"
    );
    for trigger in [
        "\n  push:",
        "\n  pull_request:",
        "\n  workflow_dispatch:",
        "\n  workflow_run:",
        "\n  repository_dispatch:",
        "\n  schedule:",
    ] {
        assert!(
            !workflow.contains(trigger),
            "audit gained trigger {trigger}"
        );
    }
    for authority in [
        "environment:",
        "RMUX_POLICY_AUDIT_APP_PRIVATE_KEY",
        "actions/create-github-app-token@",
        "contents: write",
        "id-token: write",
        "attestations: write",
        "packages: write",
        "self-hosted",
        "PAT_TOKEN",
    ] {
        assert!(!workflow.contains(authority), "audit gained {authority}");
    }

    let capabilities = activation["capabilities"]
        .as_object()
        .expect("activation capabilities");
    assert_eq!(capabilities.len(), 6);
    assert!(capabilities.values().all(|value| value == false));
    assert_eq!(activation["runtime_override_allowed"], false);
    assert_eq!(activation["status"], "disarmed");
    assert_eq!(contract["audit_app"]["configured"], true);
    assert_eq!(contract["audit_app"]["app_id"], 4344532);
    assert_eq!(contract["audit_app"]["installation_id"], 147749910);
    assert_eq!(reference_schema["properties"]["app_id"]["const"], 4344532);
    assert_eq!(
        reference_schema["properties"]["installation_id"]["const"],
        147749910
    );
    assert_eq!(contract["audit_app"]["pat_fallback"], false);
    assert_eq!(contract["workflow"]["caller_count"], 2);
    assert_eq!(
        contract["workflow"]["privileged_job_condition"],
        "disabled-in-promoter"
    );
    assert_eq!(contract["token_lifecycle"]["collector_methods"][0], "GET");
    let required_checks = contract["expected_state"]["main"]["required_checks"]
        .as_array()
        .expect("required check bindings");
    assert_eq!(required_checks.len(), 15);
    assert!(required_checks.iter().all(|check| {
        check["app_id"] == 15368
            && check["context"]
                .as_str()
                .is_some_and(|context| !context.is_empty())
    }));

    let workflows = repo_root().join(".github/workflows");
    let mut callers = Vec::new();
    for entry in fs::read_dir(workflows).expect("list workflows") {
        let path = entry.expect("workflow entry").path();
        if path.ends_with("release-policy-audit.yml") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read workflow");
        if text.contains("uses: ./.github/workflows/release-policy-audit.yml") {
            callers.push(path);
        }
    }
    callers.sort();
    assert_eq!(callers.len(), 2, "policy audit must have two exact callers");
    assert!(callers[0].ends_with("release-promote.yml"));
    assert!(callers[1].ends_with("release-promotion-simulation.yml"));
    for caller in &callers {
        let text = fs::read_to_string(caller).expect("read policy audit caller");
        assert_eq!(
            text.matches("uses: ./.github/actions/release-policy-audit")
                .count(),
            1
        );
        assert!(text.contains("environment: release-policy-audit"));
        assert!(text
            .contains("audit-app-private-key: ${{ secrets.RMUX_POLICY_AUDIT_APP_PRIVATE_KEY }}"));
        assert!(!text.contains("secrets: inherit"));
    }
    let caller = fs::read_to_string(&callers[0]).expect("read promote workflow");
    let audit_job = caller
        .split("\n  policy-audit:\n")
        .nth(1)
        .and_then(|tail| tail.split("\n  authorize-promotion:\n").next())
        .expect("isolated policy audit caller job");
    assert!(audit_job.contains("if: ${{ false }}"));
    let simulation = fs::read_to_string(&callers[1]).expect("read simulation workflow");
    assert!(simulation.contains("on:\n  workflow_dispatch:"));
    assert!(simulation.contains("simulation: true"));
    assert!(simulation.contains("publication_authority\": False"));
    assert!(simulation.contains("repository_mutations\": False"));
    for authority in [
        "contents: write",
        "id-token: write",
        "attestations: write",
        "packages: write",
    ] {
        assert!(
            !simulation.contains(authority),
            "simulation gained {authority}"
        );
    }

    let collector = include_str!("../scripts/release/policy-audit.py");
    assert!(collector.contains("method=\"GET\""));
    assert!(!collector.contains("method=\"POST\""));
    assert!(!collector.contains("method=\"PUT\""));
    assert!(!collector.contains("method=\"PATCH\""));
    assert!(!collector.contains("method=\"DELETE\""));
    assert!(!collector.contains("data="));
}

#[cfg(unix)]
fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-policy-audit-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create policy fixture directory");
    path
}

#[cfg(unix)]
fn run_python(script: &str, root: &PathBuf) -> Output {
    Command::new("python3")
        .args(["-c", script])
        .arg(repo_root())
        .arg(root)
        .output()
        .expect("run policy audit fixture")
}

#[test]
#[cfg(unix)]
fn every_release_capability_fails_closed() {
    let guard = repo_root().join("scripts/release/assert-release-capability.py");
    for capability in [
        "downstream_channels",
        "github_release_publication",
        "policy_audit",
        "promotion_authorization",
        "publication_receipt",
        "signed_tag_creation",
    ] {
        let output = Command::new(&guard)
            .arg(capability)
            .current_dir(repo_root())
            .output()
            .expect("run capability guard");
        assert!(
            !output.status.success(),
            "{capability} unexpectedly enabled"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("disabled until reviewed PR8"));
    }
}

#[test]
#[cfg(unix)]
fn release_activation_is_atomic_and_has_no_partial_state() {
    let root = temp_dir("activation");
    let ledger = root.join("release-activation.json");
    let guard = repo_root().join("scripts/release/assert-release-capability.py");
    let mut value: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/release-activation.json"))
            .expect("parse activation ledger");
    value["status"] = serde_json::json!("active");
    for enabled in value["capabilities"]
        .as_object_mut()
        .expect("capability object")
        .values_mut()
    {
        *enabled = serde_json::json!(true);
    }
    fs::write(
        &ledger,
        serde_json::to_vec_pretty(&value).expect("encode active ledger"),
    )
    .expect("write active ledger");
    for capability in value["capabilities"]
        .as_object()
        .expect("capability object")
        .keys()
    {
        let output = Command::new(&guard)
            .arg(capability)
            .args(["--ledger", ledger.to_str().expect("UTF-8 ledger path")])
            .current_dir(repo_root())
            .output()
            .expect("run active capability guard");
        assert!(
            output.status.success(),
            "{capability} rejected atomic activation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    value["capabilities"]["publication_receipt"] = serde_json::json!(false);
    fs::write(
        &ledger,
        serde_json::to_vec_pretty(&value).expect("encode partial ledger"),
    )
    .expect("write partial ledger");
    let rejected = Command::new(&guard)
        .arg("signed_tag_creation")
        .args(["--ledger", ledger.to_str().expect("UTF-8 ledger path")])
        .current_dir(repo_root())
        .output()
        .expect("run partial capability guard");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("atomically"));
    fs::remove_dir_all(root).expect("remove activation fixture");
}

#[cfg(unix)]
const AUDIT_FIXTURE: &str = r#"
import copy
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

repo, root = map(Path, sys.argv[1:])
script = repo / "scripts/release/policy-audit.py"
base_contract_path = repo / ".github/release/policy-audit-contract.json"
base_contract = json.loads(base_contract_path.read_text())
source = "0123456789abcdef0123456789abcdef01234567"
fixture = root / "api"
fixture.mkdir()

def write(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")

contract = copy.deepcopy(base_contract)
contract_path = root / "contract.json"
write(contract_path, contract)

expected = contract["expected_state"]
expected_checks = expected["main"]["required_checks"]
expected_contexts = [check["context"] for check in expected_checks]

def environment(environment_id):
    return {
        "id": environment_id, "can_admins_bypass": False,
        "deployment_branch_policy": {"protected_branches": False, "custom_branch_policies": True},
        "protection_rules": [
            {"type": "required_reviewers", "prevent_self_review": False,
             "reviewers": [{"type": "User", "reviewer": {"id": 876824, "login": "shideneyu"}}]},
            {"type": "branch_policy"},
        ],
    }

def environment_policies():
    return {"total_count": 2, "branch_policies": [
        {"id": 1, "name": "main", "type": "branch"},
        {"id": 2, "name": "v*", "type": "tag"},
    ]}

responses = {
    "repository": {
        "id": 1239918790, "full_name": "Helvesec/rmux", "default_branch": "main",
        "visibility": "public", "archived": False,
        "security_and_analysis": {
            "secret_scanning": {"status": "enabled"},
            "secret_scanning_push_protection": {"status": "enabled"},
        },
    },
    "branch_protection": {
        "enforce_admins": {"enabled": True},
        "required_status_checks": {
            "strict": True, "contexts": expected_contexts, "checks": expected_checks,
        },
        "required_pull_request_reviews": None,
        "required_signatures": {"enabled": True},
        "allow_force_pushes": {"enabled": False},
        "allow_deletions": {"enabled": False},
    },
    "tag_creation_ruleset": {
        "id": 19174415, "name": "RMUX release tag creation", "target": "tag",
        "enforcement": "active", "conditions": {"ref_name": {"include": ["refs/tags/v*"], "exclude": []}},
        "rules": [{"type": "creation"}],
        "bypass_actors": [{"actor_id": 4339867, "actor_type": "Integration", "bypass_mode": "always"}],
    },
    "tag_immutability_ruleset": {
        "id": 18792083, "name": "immutable RMUX release tags", "target": "tag",
        "enforcement": "active", "conditions": {"ref_name": {"include": ["refs/tags/v*"], "exclude": []}},
        "rules": [{"type": "update"}, {"type": "deletion"}], "bypass_actors": [],
    },
    "release_environment": environment(16229050415),
    "release_environment_policies": environment_policies(),
    "release_policy_audit": environment(18412850020),
    "release_policy_audit_policies": environment_policies(),
    "release_publication": environment(18412849790),
    "release_publication_policies": environment_policies(),
    "release_tagging": environment(18400330964),
    "release_tagging_policies": environment_policies(),
    "immutable_releases": {"enabled": True, "enforced_by_owner": True},
    "self_hosted_runners": {"total_count": 0, "runners": []},
    "audit_app": {
        "id": 4344532, "slug": "helvesec-rmux-policy-audit",
        "owner": {"login": "Helvesec"}, "events": [],
        "permissions": {"actions": "read", "administration": "write", "metadata": "read"},
    },
    "installation_repositories": {
        "total_count": 1, "repositories": [{"id": 1239918790, "full_name": "Helvesec/rmux"}],
    },
    "policy_preparer_workflow": copy.deepcopy(expected["policy_preparer_workflow"]),
    "policy_promoter_workflow": copy.deepcopy(expected["policy_promoter_workflow"]),
    "policy_simulation_workflow": copy.deepcopy(expected["policy_simulation_workflow"]),
}
for key, value in responses.items():
    write(fixture / f"{key}.json", value)

policy = root / "policy.json"
write(policy, {
    "source_git_sha": source, "release_policy_sha256": "11" * 32,
    "contract_blob_oid": "22" * 20,
})
predicate = root / "predicate.json"
common = [
    sys.executable, script, "collect", "--repository", "Helvesec/rmux",
    "--source-sha", source, "--candidate-run-id", "77", "--candidate-run-attempt", "1",
    "--candidate-manifest-run-id", "88", "--candidate-manifest-run-attempt", "1",
    "--candidate-manifest-artifact-id", "99",
    "--candidate-manifest-artifact-digest", "sha256:" + "33" * 32,
    "--candidate-manifest-sha256", "44" * 32,
    "--candidate-manifest-created-at", "2026-07-19T11:00:00Z",
    "--candidate-manifest-expires-at", "2026-07-20T11:00:00Z",
    "--release-intent-id", "stable:policy:test", "--planned-release-ref", "v0.9.1",
    "--release-kind", "stable", "--audit-run-id", "111", "--audit-run-attempt", "1",
    "--audit-workflow-id", "316591947",
    "--audit-workflow-path", ".github/workflows/release-promotion-simulation.yml",
    "--audit-app-id", "4344532", "--audit-installation-id", "147749910",
    "--audit-app-slug", "helvesec-rmux-policy-audit",
    "--contract", contract_path, "--policy-root", policy,
    "--api-fixture-dir", fixture, "--now", "2026-07-19T12:00:00Z", "--output", predicate,
]

def invoke(arguments=common):
    return subprocess.run([str(value) for value in arguments], cwd=repo, capture_output=True, text=True)

accepted = invoke()
assert accepted.returncode == 0, accepted.stderr

# The live collector has no contract-controlled path input. It can only issue
# the exact ordered GETs compiled into policy_audit_contract.py.
sys.path.insert(0, str(script.parent))
module_spec = importlib.util.spec_from_file_location("rmux_policy_audit", script)
audit_module = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(audit_module)
observed_paths = []
audit_module.get_json = lambda path, token: observed_paths.append(path) or {}
os.environ["RMUX_POLICY_AUDIT_TOKEN"] = "offline-test-token"
live_responses = audit_module.load_responses(None)
assert list(live_responses) == [item["key"] for item in contract["api_gets"]]
assert observed_paths == [item["path"] for item in contract["api_gets"]]

def reject_get_contract(label, mutate):
    changed = copy.deepcopy(contract)
    mutate(changed["api_gets"])
    write(contract_path, changed)
    result = invoke()
    write(contract_path, contract)
    assert result.returncode != 0, f"{label} GET allowlist mutation accepted"
    assert "exact ordered allowlist" in result.stderr, result.stderr

for index in range(len(contract["api_gets"])):
    reject_get_contract(
        f"key-{index}",
        lambda gets, index=index: gets[index].update(key=f"mutated_{index}"),
    )
    reject_get_contract(
        f"path-{index}",
        lambda gets, index=index: gets[index].update(path=f"/attacker/{index}"),
    )
    reject_get_contract(
        f"remove-{index}",
        lambda gets, index=index: gets.pop(index),
    )
reject_get_contract(
    "addition",
    lambda gets: gets.append({"key": "unexpected", "path": "/attacker"}),
)
reject_get_contract(
    "reordering",
    lambda gets: gets.__setitem__(slice(0, 2), reversed(gets[0:2])),
)

verify = invoke([
    sys.executable, script, "verify", "--predicate", predicate,
    "--contract", contract_path, "--now", "2026-07-19T12:14:59Z",
])
assert verify.returncode == 0, verify.stderr
reference = root / "reference.json"
created_reference = invoke([
    sys.executable, script, "reference", "--predicate", predicate,
    "--predicate-artifact-id", "123", "--predicate-artifact-digest", "sha256:" + "55" * 32,
    "--output", reference,
])
assert created_reference.returncode == 0, created_reference.stderr
checked_reference = invoke([
    sys.executable, script, "verify-reference", "--predicate", predicate,
    "--reference", reference, "--contract", contract_path, "--now", "2026-07-19T12:10:00Z",
])
assert checked_reference.returncode == 0, checked_reference.stderr

if importlib.util.find_spec("jsonschema"):
    import jsonschema
    for name, document in [
        ("policy-audit-predicate.schema.json", predicate),
        ("policy-audit-reference.schema.json", reference),
    ]:
        schema = json.loads((repo / ".github/release/schemas" / name).read_text())
        jsonschema.Draft202012Validator(schema).validate(json.loads(document.read_text()))

disabled = copy.deepcopy(base_contract)
disabled["audit_app"].update({
    "configured": False, "app_id": None, "installation_id": None, "app_slug": None,
})
disabled_path = root / "disabled-contract.json"
write(disabled_path, disabled)
disabled_args = list(common)
disabled_args[disabled_args.index(contract_path)] = disabled_path
rejected_disabled = invoke(disabled_args)
assert rejected_disabled.returncode != 0
assert "unconfigured" in rejected_disabled.stderr

wrong_repo = list(common)
wrong_repo[wrong_repo.index("Helvesec/rmux")] = "attacker/fork"
assert invoke(wrong_repo).returncode != 0

for flag, replacement in [
    ("--audit-workflow-id", "1"),
    ("--audit-workflow-path", ".github/workflows/release-promote.yml"),
    ("--audit-app-id", "1"),
    ("--audit-installation-id", "2"),
    ("--audit-app-slug", "attacker-app"),
]:
    changed = list(common)
    changed[changed.index(flag) + 1] = replacement
    rejected = invoke(changed)
    assert rejected.returncode != 0, f"{flag} identity drift accepted"
    assert "identity" in rejected.stderr, rejected.stderr

def reject_response(key, mutate, expected_message):
    original = copy.deepcopy(responses[key])
    changed = copy.deepcopy(original)
    mutate(changed)
    write(fixture / f"{key}.json", changed)
    result = invoke()
    write(fixture / f"{key}.json", original)
    assert result.returncode != 0, f"{key} mutation accepted"
    assert expected_message in result.stderr, result.stderr

reject_response("repository", lambda v: v["security_and_analysis"]["secret_scanning"].update(status="disabled"), "repository differs")
reject_response("branch_protection", lambda v: v["enforce_admins"].update(enabled=False), "main differs")
reject_response("branch_protection", lambda v: v["required_status_checks"]["checks"][0].update(app_id=1), "main differs")
reject_response("branch_protection", lambda v: v["required_status_checks"]["checks"][0].update(app_id=None), "app_id must be positive")
reject_response("branch_protection", lambda v: v["required_status_checks"]["checks"][0].update(app_id=-1), "app_id must be positive")
reject_response("branch_protection", lambda v: v["required_status_checks"].pop("checks"), "App bindings are unavailable")
reject_response("branch_protection", lambda v: v["required_status_checks"]["checks"].pop(), "contexts and App bindings differ")
reject_response("branch_protection", lambda v: v["required_status_checks"]["checks"].append(copy.deepcopy(v["required_status_checks"]["checks"][0])), "contexts must be unique")
reject_response("branch_protection", lambda v: v["required_status_checks"]["contexts"].__setitem__(0, "attacker/check"), "contexts and App bindings differ")
reject_response("tag_creation_ruleset", lambda v: v["bypass_actors"][0].update(actor_id=1), "tag_creation_ruleset differs")
reject_response("tag_creation_ruleset", lambda v: v.pop("bypass_actors"), "bypass actors must be an array")
reject_response("tag_immutability_ruleset", lambda v: v["bypass_actors"].append({"actor_id": 1}), "tag_immutability_ruleset differs")
reject_response("release_environment", lambda v: v.update(can_admins_bypass=True), "release differs")
reject_response("release_policy_audit", lambda v: v.update(can_admins_bypass=True), "release_policy_audit differs")
reject_response("release_publication", lambda v: v.update(can_admins_bypass=True), "release_publication differs")
reject_response("release_tagging", lambda v: v.update(can_admins_bypass=True), "release_tagging differs")
reject_response("release_environment_policies", lambda v: v["branch_policies"][1].update(name="attacker/*"), "release differs")
reject_response("self_hosted_runners", lambda v: v.update(total_count=1, runners=[{"id": 1}]), "inventory must be empty")
reject_response("audit_app", lambda v: v.update(id=1), "action identity differs")
reject_response("audit_app", lambda v: v["owner"].update(login="attacker"), "owner changed")
reject_response("audit_app", lambda v: v.update(events=["push"]), "webhook events changed")
reject_response("installation_repositories", lambda v: v.update(total_count=2, repositories=[{"id": 1}, {"id": 1239918790}]), "audit_app_repositories differs")
reject_response("policy_preparer_workflow", lambda v: v.update(state="disabled_manually"), "policy_preparer_workflow differs")
reject_response("policy_promoter_workflow", lambda v: v.update(state="disabled_manually"), "policy_promoter_workflow differs")
reject_response("policy_simulation_workflow", lambda v: v.update(state="disabled_manually"), "policy_simulation_workflow differs")

document = json.loads(predicate.read_text())
mutated = copy.deepcopy(document)
mutated["observed_state"]["main"]["enforce_admins"] = False
write(predicate, mutated)
bad_predicate = invoke([
    sys.executable, script, "verify", "--predicate", predicate, "--contract", contract_path,
])
assert bad_predicate.returncode != 0 and "main differs" in bad_predicate.stderr
write(predicate, document)

expired = invoke([
    sys.executable, script, "verify", "--predicate", predicate,
    "--contract", contract_path, "--now", "2026-07-19T12:15:00Z",
])
assert expired.returncode != 0 and "expired" in expired.stderr

reference_value = json.loads(reference.read_text())
reference_value["predicate_sha256"] = "66" * 32
write(reference, reference_value)
bad_reference = invoke([
    sys.executable, script, "verify-reference", "--predicate", predicate,
    "--reference", reference, "--contract", contract_path, "--now", "2026-07-19T12:10:00Z",
])
assert bad_reference.returncode != 0 and "exact predicate" in bad_reference.stderr
"#;

#[test]
#[cfg(unix)]
fn policy_audit_accepts_exact_state_and_rejects_adversarial_mutations() {
    let root = temp_dir("adversarial");
    let output = run_python(AUDIT_FIXTURE, &root);
    let _ = fs::remove_dir_all(&root);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
