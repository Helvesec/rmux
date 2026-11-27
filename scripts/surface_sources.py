"""Project identities and metadata from the owning registries, without Cargo.

Stable means a current registered contract, not a 1.0 release verdict. Internal
packages are outside the facade contract. Reserved wire ids and retired
capabilities remain visible as absent. No generated manifest is an input here.
"""
import ast
import hashlib
import importlib.util
import json
from pathlib import Path

import toml_reader
from surface_conformance import rust_modules
from surface_syntax import (Token, array, atom, calls, function, group, initializer, literal, matches,
                            optional_string, parse, require, split, strings, text)


CORE = "crates/rmux-core/src/"
PROTO = "crates/rmux-proto/"
OUTER = "crates/rmux-server/src/outer_terminal/"


class Sources:
    def __init__(self, root):
        self.root = Path(root).resolve()
        self.inputs = {}
        self.cache = {}

    def binary(self, name):
        relative = Path(name)
        require(not relative.is_absolute() and ".." not in relative.parts,
                "source path escapes root: " + name)
        path = self.root / relative
        require(path.resolve().is_relative_to(self.root) and path.is_file(),
                "orphan source: " + name)
        raw = path.read_bytes()
        self.inputs[name] = hashlib.sha256(raw).hexdigest()
        return raw

    def read(self, name):
        return self.binary(name).decode("utf-8")

    def rust(self, name):
        if name not in self.cache:
            self.cache[name] = parse(self.read(name))
        return self.cache[name]

    def toml(self, name):
        return toml_reader.loads(self.read(name))

    def json(self, name):
        def pairs(items):
            result = {}
            for key, value in items:
                require(key not in result, "duplicate JSON key: " + key)
                result[key] = value
            return result
        return json.loads(self.read(name), object_pairs_hook=pairs)


def case(path, name):
    return {"path": path, "case": name}


def entry(family, name, sources, metadata, cases, classification="stable"):
    return dict(family=family, id=name, classification=classification,
                sources=sorted(sources), metadata=metadata, conformance=cases)


def unique(values, label):
    require(len(values) == len(set(values)), "duplicate " + label)


def enum(sources, path, name):
    body = group(sources.rust(path), ["pub", "enum", name], "{")
    rows = split(body)
    names = [atom(row[:1]) for row in rows]
    unique(names, name)
    return rows


def commands(sources):
    path = CORE + "command_schema.rs"
    macro = group(sources.rust(path), ["macro_rules", "!", "command_schema"], "{")
    expansion = group(macro, ["=", ">"], "{")
    body = group(expansion, ["$", "consumer", "!"], "{")
    require(len(body) == 8 and [atom([body[i]]) for i in (0, 2, 4, 6)] ==
            ["commands", "delegated", "local", "aliases"], "unclassified command section")
    cases = [case(CORE + "command_inventory.rs", "signatures_follow_parser_table_then_rmux_extensions")]
    result, aliases = [], []
    for section, block in zip(("commands", "delegated", "local", "aliases"), body[1::2]):
        require(block.kind == "{", "unsupported command section")
        for row in split(block.value, ";"):
            require(len(row) == 1 and row[0].kind == "(", "unclassified command row")
            fields = split(row[0].value)
            name = literal(fields[0])
            if section in ("commands", "delegated"):
                require(len(fields) == (8 if section == "commands" else 6), "command fields")
                kind = atom(fields[2])
                require(kind in ("tmux", "extension"), "unclassified command kind: " + kind)
                alias = optional_string(fields[1])
                usage = literal(fields[4])
                metadata = dict(kind=kind, usage=usage, declaration=text(row[0].value))
                if alias:
                    aliases.append((alias, name, "short"))
            elif section == "local":
                require(len(fields) == 3, "local command fields")
                metadata = dict(kind="local", usage=literal(fields[2]))
            else:
                require(len(fields) == 3, "alias fields")
                aliases.append((name, literal(fields[1]).split()[0], "expansion"))
                metadata = dict(kind="expansion-alias", expansion=literal(fields[1]))
                continue
            result.append(entry("commands", name, [path], metadata, cases))
    names = {e["id"] for e in result}
    for alias, target, kind in aliases:
        require(target in names, "orphan command alias: " + alias)
        result.append(entry("commands", alias, [path], dict(kind=kind + "-alias", target=target), cases))
    return result


OPTION_SECTIONS = ("server", "session", "session_status", "window", "window_display")


def options(sources):
    table = CORE + "options/registry/table.rs"
    wire = PROTO + "src/types/options.rs"
    variants = {atom(row) for row in enum(sources, wire, "OptionName")}
    result, typed = [], []
    cases = [case(CORE + "options/tests/registry_metadata.rs", "option_lookup_indexes_match_registry_metadata")]
    # table.rs concatenates the OPTIONS arrays of its section modules in this order.
    paths = [CORE + "options/registry/table/" + section + ".rs" for section in OPTION_SECTIONS]
    rows = [(path, row) for path in paths for row in split(array(sources.rust(path), "OPTIONS"))]
    for path, row in rows:
        require(len(row) == 2, "unclassified option row")
        if row[0] == Token("atom", "option") and row[1].kind == "(":
            fields = split(row[1].value)
            require(len(fields) == 11 and matches(fields[0][:3], ["OptionName", ":", ":"]),
                    "unclassified option row")
            variant = atom(fields[0][3:])
            typed.append(variant)
        else:
            require(row[0] == Token("atom", "OptionMetadata") and row[1].kind == "{",
                    "unclassified option declaration")
            members = split(row[1].value)
            keys = [atom(f[:1]) for f in members]
            require(keys == ["wire_option", "name", "aliases", "show_mask", "scope_mask", "global_root",
                             "value_type", "default", "separator", "is_array", "effects"]
                    and all(matches(f[1:2], [":"]) for f in members), "option metadata fields")
            fields = [f[2:] for f in members]
            require(matches(fields[0], ["None"]), "unclassified named-only option")
            variant = None
        name, aliases = literal(fields[1]), strings(fields[2])
        metadata = dict(wire=variant, aliases=aliases, declaration=[text(f) for f in fields[3:]])
        result.append(entry("options", name, [table, path, wire], metadata, cases))
        for alias in aliases:
            result.append(entry("options", alias, [table, path], dict(kind="alias", target=name), cases))
    unique(typed, "option wire key")
    require(set(typed) == variants, "orphan option wire key or registry entry")
    return result


def formats(sources):
    path = CORE + "formats/registry.rs"
    body = group(sources.rust(path), ["format_registry", "!"], "{")
    sections = split(body)
    require(len(sections) == 3, "unclassified format section")
    cases = [case(CORE + "formats/tests/tables_time.rs", "registered_descriptors_have_unique_names_and_metadata")]
    contexts = {atom(row) for row in enum(sources, path, "FormatVariableContext")}
    variable_path = CORE + "formats/variable.rs"
    variables = {atom(row) for row in enum(sources, variable_path, "FormatVariable")}
    result, keys, time, bindings = [], {}, [], []
    for section, expected in zip(sections, ("time", "tmux", "extensions")):
        require(len(section) == 3 and matches(section[:2], [expected, ":"])
                and section[2].kind == "[", "unclassified format section")
        if expected == "time":
            time = [atom(row) for row in split(section[2].value)]
            unique(time, "time format key")
            continue
        for row in split(section[2].value, ";"):
            require(len(row) == 4 and matches(row[1:3], ["=", ">"])
                    and row[3].kind == "(", "unclassified format row")
            fields = split(row[3].value)
            require(len(fields) == (4 if expected == "tmux" else 5), "format fields")
            key, name, provider = atom(row[:1]), literal(fields[0]), atom(fields[2])
            require(key not in keys, "duplicate format key: " + key)
            require(provider in ("Core", "Runtime", "BuildCapability", "Option", "Command", "KnownEmpty"),
                    "unclassified format provider: " + provider)
            require(len(fields[1]) == 3 and matches(fields[1][:2], ["contexts", "!"])
                    and fields[1][2].kind == "(", "unclassified format contexts")
            attached = [atom(row) for row in split(fields[1][2].value, "|")]
            require(attached and set(attached) <= contexts, "orphan format context")
            if not matches(fields[3], ["None"]):
                require(len(fields[3]) == 2 and fields[3][0] == Token("atom", "Some")
                        and fields[3][1].kind == "(", "unclassified format binding")
                bound = fields[3][1].value
                require(matches(bound[:3], ["FormatVariable", ":", ":"]), "unclassified format binding")
                bindings.append(atom(bound[3:]))
            keys[key] = name
            metadata = dict(key=key, inventory=expected, provider=provider,
                            contexts=text(fields[1]), binding=text(fields[3]))
            # KnownEmpty is a supported empty fallback, not an absent format name.
            result.append(entry("formats", name, [path, variable_path], metadata, cases))
    require(set(time) <= keys.keys(), "orphan time format key")
    unique(bindings, "format enum binding")
    require(set(bindings) == variables, "orphan format enum binding")
    for record in result:
        record["metadata"]["time"] = record["metadata"]["key"] in time
    return result


def wire_ledger(sources, path):
    tokens = sources.rust(path)
    value = initializer(tokens, "V1_FRAME_LEDGER")
    if len(value) == 2 and value[1].kind == "[":
        return [(path, fields) for fields in calls(array(tokens, "V1_FRAME_LEDGER"), "entry")]
    require(value == parse("&assemble_ledger()"), "unsupported wire ledger initializer")
    # Recognize this bounded concatenation, never evaluate arbitrary Rust. A
    # changed assembly algorithm must fail closed until the reader supports it.
    require(function(tokens, "ledger_len") == parse("""
        let mut len = 0;
        let mut section = 0;
        while section < LEDGER_SECTIONS.len() {
            len += LEDGER_SECTIONS[section].len();
            section += 1;
        }
        len
    """), "unsupported wire ledger length")
    require(function(tokens, "assemble_ledger") == parse("""
        let mut entries = [request_commands::ENTRIES[0]; ledger_len()];
        let mut section = 0;
        let mut offset = 0;
        while section < LEDGER_SECTIONS.len() {
            let source = LEDGER_SECTIONS[section];
            let mut index = 0;
            while index < source.len() {
                entries[offset] = source[index];
                offset += 1;
                index += 1;
            }
            section += 1;
        }
        entries
    """), "unsupported wire ledger assembly")
    modules = rust_modules(tokens)
    result, sections = [], []
    for row in split(array(tokens, "LEDGER_SECTIONS")):
        require(len(row) == 4 and matches(row[1:], [":", ":", "ENTRIES"]),
                "unsupported wire ledger section")
        name = atom(row[:1])
        require(modules.count((name, None)) == 1
                and parse("mod " + name + ";") in tuple(tokens[i:i+3] for i in range(len(tokens)-2)),
                "orphan or unsupported wire ledger module: " + name)
        sections.append(name)
        section_path = str(Path(path).with_suffix("") / (name + ".rs")).replace("\\", "/")
        result.extend((section_path, fields)
                      for fields in calls(array(sources.rust(section_path), "ENTRIES"), "entry"))
    unique(sections, "wire ledger section")
    require(sections, "empty wire ledger sections")
    return result


def wire(sources):
    path = PROTO + "src/frame_kind.rs"
    cases = [case(PROTO + "tests/wire_ledger_v1.rs", "ledger_active_size_matches_request_and_response_variant_count")]
    result, active = [], {"c2s": [], "s2c": []}
    for section_path, fields in wire_ledger(sources, path):
        require(len(fields) == 7 and len(fields[0]) == 2 and fields[0][1].kind == "(", "wire fields")
        direction = atom(fields[0][:1])
        require(direction in active, "unclassified wire direction")
        tag = int(atom(fields[0][1].value).replace("_", ""), 0)
        require(0 <= tag <= 32767, "wire tag exceeds direction partition")
        expected = "ClientToServer" if direction == "c2s" else "ServerToClient"
        require(matches(fields[1], ["FrameDirection", ":", ":", expected]), "wire direction mismatch")
        status, dto, fixture = atom(fields[2]), literal(fields[3]), optional_string(fields[5])
        require(status in ("ACTIVE", "RESERVED"), "unclassified wire status")
        inputs = sorted({path, section_path})
        if fixture:
            fixture_path = "tests/wire-fixtures/ledger-v1-current-wire/" + fixture + ".bin"
            # Binary fixtures are identities too; do not decode them as text.
            sources.binary(fixture_path)
            inputs.append(fixture_path)
        if status == "ACTIVE":
            active[direction].append(dto)
        result.append(entry("wire", f"{direction}:{tag:05d}", inputs,
                            dict(dto=dto, status=status, feature=text(fields[4]), fixture=fixture,
                                 note=literal(fields[6])), cases,
                            "stable" if status == "ACTIVE" else "absent"))
    unique([record["id"] for record in result], "wire kind")
    by_id = {record["id"]: record["metadata"] for record in result}
    for direction, name, filename in (("c2s", "Request", "request.rs"), ("s2c", "Response", "response.rs")):
        rows = enum(sources, PROTO + "src/" + filename, name)
        dtos, payloads = [], {}
        for row in rows:
            require(len(row) == 2 and row[1].kind == "(", "unsupported wire enum variant")
            payload = row[1].value
            if matches(payload[:2], ["Box", "<"]) and matches(payload[-1:], [">"]):
                payload = payload[2:-1]
            if matches(payload[:3], ["crate", ":", ":"]):
                payload = payload[3:]
            dtos.append(atom(payload))
            payloads[atom(row[:1])] = atom(payload)
        unique(active[direction], "wire DTO")
        unique(dtos, "wire enum payload")
        require(set(active[direction]) == set(dtos), "orphan wire DTO: " + direction)
        helper = function(sources.rust(path), "frame_kind_for_" + name.lower())
        arms = group(helper, ["match", name.lower()], "{")
        variants = []
        for arm in split(arms):
            require(len(arm) == 9 and matches(arm[:3], [name, ":", ":"])
                    and arm[4].kind == "(" and matches(arm[4].value, ["_"])
                    and matches(arm[5:8], ["=", ">", direction]) and arm[8].kind == "(",
                    "unclassified wire helper")
            variant = atom(arm[3:4])
            tag = int(atom(arm[8].value).replace("_", ""), 0)
            identity = f"{direction}:{tag:05d}"
            require(variant in payloads and identity in by_id
                    and by_id[identity]["status"] == "ACTIVE"
                    and by_id[identity]["dto"] == payloads[variant], "orphan wire helper: " + variant)
            variants.append(variant)
        unique(variants, "wire helper variant")
        require(set(variants) == set(payloads), "orphan wire helper variant")
    declared = {r["metadata"]["fixture"] + ".bin" for r in result if r["metadata"]["fixture"]}
    retained = {p.name for p in (sources.root / "tests/wire-fixtures/ledger-v1-current-wire").glob("*.bin")}
    require(declared == retained, "orphan wire fixture")
    return result


def capabilities(sources):
    path = PROTO + "src/capabilities/registry.rs"
    body = group(sources.rust(path), ["capabilities", "!"], "{")
    require(len(body) == 6, "unclassified capability section")
    cases = [case(PROTO + "tests/capability_registry.rs", "registry_projects_advertisement_negotiation_and_stable_error_codes")]
    policies = [{atom(row) for row in enum(sources, path, "Capability" + kind)}
                for kind in ("Advertiser", "Direction", "Scope", "Lifetime", "Negotiation", "Fallback")]
    result, constants = [], []
    for label, block in zip(body[::2], body[1::2]):
        mode = atom([label])
        require(mode in ("Baseline", "Web", "Retired") and block.kind == "{", "unclassified capability mode")
        tokens = list(block.value)
        while tokens:
            fields = tokens[:4]
            require(len(fields) == 4 and fields[1] == Token("atom", "=")
                    and fields[3] == Token("atom", ";"), "unclassified capability row")
            constant, name = atom(fields[:1]), literal(fields[2:3])
            constants.append(constant)
            require(len(tokens) >= 6 and tokens[4].kind == "(" and tokens[5] == Token("atom", ";"),
                    "missing capability policy")
            policy = split(tokens[4].value)
            require(len(policy) == 6, "capability policy fields")
            require(all(atom(field) in allowed for field, allowed in zip(policy, policies)),
                    "orphan capability policy")
            result.append(entry("capabilities", name, [path], dict(constant=constant, advertisement=mode,
                                policy=[atom(f) for f in policy]), cases,
                                "absent" if mode == "Retired" else "stable"))
            del tokens[:6]
    unique(constants, "capability constant")
    cli, scripting = "src/cli/capabilities.rs", "src/cli/scripting_contract.rs"
    report = function(sources.rust(cli), "capabilities_report")
    extras = group(report, [".", "chain"], "(")
    require(len(extras) == 1 and extras[0].kind == "[", "unclassified CLI capability projection")
    command_names = [literal(r) for r in split(array(sources.rust(scripting), "JSON_COMMANDS"))]
    notifications = [literal(r) for r in split(array(sources.rust(scripting), "CONTROL_NOTIFICATIONS"))]
    unique(command_names, "JSON command")
    unique(notifications, "control notification")
    command_ids = {e["id"] for e in commands(sources)}
    require(set(command_names) <= command_ids, "orphan scripting command")
    for row in split(extras[0].value):
        result.append(entry("capabilities", literal(row), [cli, scripting],
                            dict(advertisement="CLI", json_commands=command_names, control_notifications=notifications),
                            [case("tests/scripting_sdk.rs", "capabilities_json_advertises_binary_contract")]))
    return result


def packages(sources):
    workspace = sources.toml("Cargo.toml")
    members = workspace["workspace"]["members"]
    unique(members, "workspace member")
    # The existing facade policy owns the public/internal distinction.
    policy_path = "scripts/public_api_policy.py"
    assignments = [n for n in ast.parse(sources.read(policy_path)).body
                   if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "FACADES" for t in n.targets)]
    require(len(assignments) == 1, "missing facade policy")
    facades = ast.literal_eval(assignments[0].value)
    result = []
    cases = [case("tests/public_api_gate_spec.py", "test_missing_crate_feature_drift_or_library_is_not_green")]
    for member in members:
        require(not any(c in member for c in "*?["), "unsupported workspace member glob")
        path = (Path(member) / "Cargo.toml").as_posix()
        manifest = sources.toml(path)
        package = dict(manifest["package"])
        name = package["name"]
        for key, value in list(package.items()):
            if isinstance(value, dict) and value == {"workspace": True}:
                package[key] = workspace["workspace"]["package"][key]
        metadata = dict(package=package, features=manifest.get("features", {}),
                        library=manifest.get("lib"), binaries=manifest.get("bin", []),
                        facade=name in facades)
        if name in facades:
            require(metadata["features"] == facades[name], "unclassified facade feature change: " + name)
        result.append(entry("packages", "crate:" + name, ["Cargo.toml", path, policy_path] if path != "Cargo.toml"
                            else [path, policy_path], metadata, cases if name in facades else [],
                            "stable" if name in facades else "post-1.0"))
    require({"crate:" + n for n in facades} <= {e["id"] for e in result}, "orphan public facade")
    descriptor_path = "scripts/packaging/rmux-package-layout.json"
    descriptor = sources.json(descriptor_path)
    validator_path = "scripts/packaging/layout.py"
    sources.read(validator_path)
    spec = importlib.util.spec_from_file_location("surface_package_layout", Path(__file__).parent / "packaging/layout.py")
    layout = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(layout)
    identifiers = [r["id"] for r in descriptor["entries"]]
    unique(identifiers, "package layout entry")
    require(descriptor["schema"] == 1 and descriptor["variants"], "unclassified package layout")
    for row in descriptor["entries"]:
        require(row["variants"] and set(row["variants"]) <= descriptor["variants"].keys(), "orphan package variant")
    for variant, settings in descriptor["variants"].items():
        try:
            layout.validate_variant(descriptor, variant)
        except layout.LayoutError as error:
            raise ValueError(str(error)) from error
        for row, placement in layout.entries(descriptor, variant):
            result.append(entry("packages", "layout:" + variant + "/" + row["id"],
                                [descriptor_path, validator_path],
                                dict(variant=settings, source=row["source"], role=row["role"], placement=placement),
                                [case("tests/package_layout_spec.py", "test_" + variant.replace("-", "_") + "_projects_the_accepted_layout")]))
    return result


def terminals(sources):
    path, consumer = OUTER + "defaults.rs", OUTER + "features.rs"
    tokens = sources.rust(path)
    names = [t.value for i, t in enumerate(tokens) if i and tokens[i-1] == Token("atom", "const")
             and t.kind == "atom" and t.value.endswith("_FEATURES")]
    unique(names, "terminal feature family")
    require(names, "empty terminal registry")
    # Retain the whole resolver token tree, including overlapping family tests.
    all_tokens = sources.rust(consumer)
    # impl blocks are nested; locate the method without interpreting its conditions.
    impl = group(all_tokens, ["impl", "OuterTerminal"], "{")
    method = function(impl, "apply_default_family_features")
    used = []
    def visit(nodes):
        for node in nodes:
            if node.kind in ("(", "[", "{"):
                visit(node.value)
            elif node.kind == "atom" and node.value.startswith("DEFAULT_") and node.value.endswith("_FEATURES"):
                used.append(node.value)
    visit(method)
    unique(used, "terminal family consumer")
    require(set(names) == set(used), "orphan terminal family")
    apply = function(impl, "apply_feature_name")
    arms = next(t.value for t in apply if t.kind == "{")
    supported = {t.value for t in arms if t.kind == "string"}
    for name in names:
        features = [literal(row) for row in split(array(tokens, name))]
        unique(features, "terminal feature")
        require({f.lower() for f in features} <= supported, "orphan terminal feature: " + name)
    parent = OUTER + "tests.rs"
    require(rust_modules(sources.rust(parent)).count(("family_tests", None)) == 1,
            "orphan terminal conformance module")
    return [entry("terminals", name, [path, consumer],
                  dict(features=[literal(row) for row in split(array(tokens, name))],
                       selector=text(method)),
                  [case(OUTER + "tests/family_tests.rs", name.lower())]) for name in names]


def personalities(sources):
    full, tiny = "src/cli.rs", "src/tiny_main.rs"
    def selector(path, name):
        body = function(sources.rust(path), name)
        found = []
        def visit(nodes):
            for j, node in enumerate(nodes):
                if node == Token("atom", "eq_ignore_ascii_case") and j + 1 < len(nodes):
                    found.append(literal(nodes[j+1].value))
                if node.kind in ("(", "[", "{"):
                    visit(node.value)
        visit(body)
        require(len(found) == 1, "unclassified personality selector")
        return found[0]
    tmux = selector(full, "invoked_as_tmux_argv0")
    require(selector(tiny, "invoked_as_tmux_from") == tmux, "orphan personality in tiny CLI")
    helper = "src/tiny_main/helper.rs"
    require(selector(helper, "invoked_as_tmux") == tmux, "orphan personality in full helper")
    native = sources.toml("Cargo.toml")["package"]["name"]
    cases = [case("tests/cli_long_help.rs", "long_help_preserves_personality_ordering_and_value_boundaries")]
    return [entry("personalities", name, [full, tiny, helper, "Cargo.toml"],
                  dict(selector="argv0-case-insensitive" if name == tmux else "default",
                       scope="CLI invocation; no connection personality claim"), cases)
            for name in (native, tmux)]


READERS = (commands, options, formats, wire, capabilities, packages, terminals, personalities)
