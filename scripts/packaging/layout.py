#!/usr/bin/env python3
"""Projections of the one RMUX package layout, for every packaging consumer.

`rmux-package-layout.json` declares, once, where each packaged file comes from,
where every format puts it, with which mode, and whether it is required. This
module turns that descriptor into the projections the builders, installers,
validators, generators and tests consume, so a format never keeps its own copy
of the layout:

    stage-plan       what a builder copies into its staging root
    check-tree       a staged or extracted package: omission, extra, mode
    manifest         the package-carried projection its installer reads
    install-plan     what an installer copies into a prefix
    uninstall-plan   what an uninstaller removes from a prefix
    check-installed  an installed prefix against the package it came from
    rpm-files        the %files list of the RPM spec
    homebrew-install the formula install block
    nested-exe       the archive-relative CLI path for WinGet and Chocolatey
    command-alias    the command name a downstream consumer exposes
    source-names     the files a builder prepares for one kind of source
    payload-provenance  a declared payload source bound to the binaries a tree
                     really carries, for a package assembled from binaries the
                     assembling repository did not build; the tree is named,
                     --package-root for package paths or --prefix for installed
                     destinations
    chocolatey-shims the commands Chocolatey installs, and what they point at
    layout-id        the format identity a package declares, and its legacy

Every command refuses an unknown variant, and every check reports the exact
path it refused, so no consumer can pass by staying silent.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys

DESCRIPTOR = Path(__file__).resolve().parent / "rmux-package-layout.json"


# An entry id travels into a manifest, a checkpoint name and a file name, so
# it stays a plain lowercase token.
ENTRY_ID = re.compile(r"^[a-z0-9][a-z0-9-]*$")


class LayoutError(Exception):
    """A refusal that names the package path or variant that caused it."""


def load(path=DESCRIPTOR):
    descriptor = json.loads(Path(path).read_text(encoding="utf-8"))
    if descriptor.get("schema") != 1:
        raise LayoutError(f"unsupported package layout schema: {descriptor.get('schema')}")
    return descriptor


def variant(descriptor, name):
    try:
        return descriptor["variants"][name]
    except KeyError:
        known = ", ".join(sorted(descriptor["variants"]))
        raise LayoutError(f"unknown package variant: {name} (known: {known})") from None


def entries(descriptor, name):
    """Every entry of one variant, in descriptor order."""
    variant(descriptor, name)
    for entry in descriptor["entries"]:
        placement = entry["variants"].get(name)
        if placement is not None:
            yield entry, placement


def source_path(entry, placement, roots, suffix):
    """Where a builder reads this entry from, given its source roots."""
    source = entry["source"]
    kind = source["kind"]
    if kind == "binary":
        root = roots.get("binaries")
        name = source["name"] + suffix
    elif kind == "repo":
        root = roots.get("repo")
        name = source["path"]
    elif kind == "completion":
        root = roots.get("completions")
        name = source["name"]
    elif kind == "generated":
        root = roots.get("generated")
        name = source["name"]
    elif kind == "conpty":
        root = roots.get("conpty")
        name = source["name"]
    else:
        raise LayoutError(f"unknown source kind for {entry['id']}: {kind}")
    if root is None:
        return None
    return os.path.join(root, name)


def staged_source_name(entry):
    """The file name a builder gives this entry inside its own sources tree."""
    source = entry["source"]
    if source["kind"] == "repo":
        return os.path.basename(source["path"])
    if source["kind"] == "binary":
        return source["name"]
    return source["name"]


def declared_paths(descriptor, name):
    """Every path a variant may contain, primary and alternative."""
    paths = {}
    for entry, placement in entries(descriptor, name):
        for candidate in [placement["path"], *placement.get("alternatives", [])]:
            paths.setdefault(candidate, (entry, placement))
    return paths


def symlinked_ancestor(root, relative):
    """The first directory above this path that is a symbolic link, if any."""
    root = Path(root)
    current = root
    for part in PurePosixPath(relative).parts[:-1]:
        current = current / part
        if current.is_symlink():
            return current.relative_to(root).as_posix()
    return None


def owned_file(root, relative):
    """True when the tree really holds this path as a regular file.

    Neither the leaf nor any directory above it may be a symbolic link: a
    package entry reached through a link is not the file the descriptor
    declares, and its bytes can live anywhere.
    """
    try:
        status = (Path(root) / relative).lstat()
    except OSError:
        return False
    if not stat.S_ISREG(status.st_mode):
        return False
    return symlinked_ancestor(root, relative) is None


def present_path(root, placement):
    """The path this entry really occupies, or None when it is absent."""
    for candidate in [placement["path"], *placement.get("alternatives", [])]:
        if owned_file(root, candidate):
            return candidate
    return None


def absent_reason(root, placement):
    """Why a declared entry is not owned: a link, a type, or a real absence."""
    for candidate in [placement["path"], *placement.get("alternatives", [])]:
        target = Path(root) / candidate
        if target.is_symlink():
            return f"package file is a symbolic link: {candidate}"
        ancestor = symlinked_ancestor(root, candidate)
        if ancestor is not None and os.path.lexists(target):
            return (f"package file is reached through a symbolic link directory: "
                    f"{candidate} (through {ancestor})")
        if os.path.lexists(target):
            return f"package file is not a regular file: {candidate}"
    return None


def walk_entries(root, ignored_prefixes):
    """Every path the tree carries: regular files, links and linked directories.

    os.walk never descends into a symbolic link, so a link to a directory is
    reported here as the single entry it is instead of disappearing with the
    files it would have contributed.
    """
    found = []
    root = Path(root)

    def record(path):
        relative = path.relative_to(root).as_posix()
        if any(relative.startswith(prefix) for prefix in ignored_prefixes):
            return
        if path.is_symlink():
            kind = "symbolic link"
        elif stat.S_ISREG(path.lstat().st_mode):
            kind = "file"
        else:
            kind = "not a regular file"
        found.append((relative, path, kind))

    for current, directories, files in os.walk(root):
        for name in directories:
            directory = Path(current) / name
            if directory.is_symlink():
                record(directory)
        for name in files:
            record(Path(current) / name)
    return sorted(found)


def full_mode(status):
    """The mode a package declares, special bits included."""
    return oct(stat.S_IMODE(status.st_mode))[2:].rjust(4, "0")


def check_tree(descriptor, name, root, skip=()):
    """A staged or extracted package against its variant. Returns its paths."""
    settings = variant(descriptor, name)
    root = Path(root)
    if not root.is_dir():
        raise LayoutError(f"package root is not a directory: {root}")
    problems = []
    accounted = set()
    for entry, placement in entries(descriptor, name):
        if entry["id"] in skip:
            occupied = present_path(root, placement)
            if occupied is not None:
                accounted.add(occupied)
            continue
        occupied = present_path(root, placement)
        if occupied is None:
            reason = absent_reason(root, placement)
            if reason is not None:
                problems.append(reason)
                continue
            if placement.get("optional"):
                continue
            problems.append(f"missing required package file: {placement['path']}")
            continue
        accounted.add(occupied)
        target = root / occupied
        if settings["modes"]:
            expected = placement.get("mode")
            if expected is None:
                expected = "0755" if entry["role"] == "executable" else "0644"
            # The declared mode is the whole mode: a setuid, setgid or sticky
            # bit nobody declared is a different mode, not a detail under a
            # permission mask.
            actual = full_mode(target.lstat())
            if actual != expected:
                problems.append(
                    f"package file mode is {actual}, expected {expected}: {occupied}"
                )
    if not settings["allow_unknown_paths"]:
        for relative, _path, kind in walk_entries(root, settings["ignored_prefixes"]):
            if relative in accounted:
                continue
            if kind == "file":
                problems.append(f"unexpected package file: {relative}")
            else:
                problems.append(f"unexpected package entry ({kind}): {relative}")
    if problems:
        raise LayoutError("\n".join(problems))
    return sorted(accounted)


def placement_of(descriptor, name, identifier):
    """Where one entry sits in a variant, by its descriptor id."""
    for entry, placement in entries(descriptor, name):
        if entry["id"] == identifier:
            return entry, placement
    raise LayoutError(f"the {name} variant has no entry {identifier}")


def downstream(descriptor, consumer):
    try:
        return descriptor["downstream"][consumer]
    except KeyError:
        known = ", ".join(sorted(descriptor["downstream"]))
        raise LayoutError(f"unknown downstream consumer: {consumer} (known: {known})") from None


def canonical_path(path, what):
    """A package path in the one form it may be written in.

    Two spellings of one file are one file. bin/rmux, bin/./rmux and
    bin//rmux name the same destination but compare as three, so a path that
    is not already canonical is refused here rather than normalised
    silently: the descriptor and the manifests are written by hand, and an
    equivalent spelling has no legitimate use in them.
    """
    if not isinstance(path, str) or not path:
        raise LayoutError(f"{what} is empty")
    if "\\" in path:
        raise LayoutError(f"{what} is not canonical (backslash): {path}")
    if re.match(r"^[A-Za-z]:", path):
        raise LayoutError(f"{what} is not canonical (drive): {path}")
    if path.startswith("/"):
        raise LayoutError(f"{what} is not canonical (absolute): {path}")
    if path.endswith("/"):
        raise LayoutError(f"{what} is not canonical (trailing separator): {path}")
    segments = path.split("/")
    if "" in segments:
        raise LayoutError(f"{what} is not canonical (empty segment): {path}")
    if "." in segments or ".." in segments:
        raise LayoutError(f"{what} is not canonical (relative segment): {path}")
    return path


def normalised_destination(destination):
    """The form two destinations are compared in.

    The comparison ignores case for every variant: one package is installed on
    case-sensitive and case-insensitive filesystems alike, and two entries
    that differ only by case would land on each other on half of them.
    """
    return destination.casefold()


# The topology each variant's builders impose. It is not a setting. The RPM
# builder copies its binaries, its metadata, the repository sources and the
# completions into one SOURCES directory, and its spec installs every one of
# them from there; the archive builders keep one directory per kind of source
# and read the repository where it lives. A descriptor may describe that
# topology, and it is read below to name what is prepared where, but it may
# not contradict it: detaching one kind on paper moves nothing on disk.
CONSUMER_TOPOLOGY = {
    "unix-archive": "per-kind",
    "windows-archive": "per-kind",
    "debian": "per-kind",
    "rpm": "one-directory",
}


def validate_source_spaces(settings, name):
    """The preparation spaces a variant declares, against what they hold.

    Where a variant prepares more than one kind of source in one directory,
    that directory is a flat directory of copies: everything that goes into it
    arrives under one name, and two sources of that name are one file. Saying
    otherwise - by leaving the declaration out, by leaving it empty, or by
    naming a directory that does not exist - would turn the guard off while
    the builder keeps copying both files over each other. The declaration is
    therefore answerable to the spaces the same variant declares, and a
    directory shared by several kinds has to be named in it.
    """
    spaces = settings.get("source_spaces")
    if not isinstance(spaces, dict) or not spaces:
        raise LayoutError(f"the {name} variant declares no preparation space")
    for kind, where in spaces.items():
        if not isinstance(where, str) or not where:
            raise LayoutError(
                f"the {name} variant does not name a directory for its {kind} sources")
    flattened = settings.get("flattened_source_spaces")
    if not isinstance(flattened, list) or any(not isinstance(item, str) for item in flattened):
        raise LayoutError(
            f"the {name} variant does not declare which of its preparation spaces are flattened")
    declared = set(spaces.values())
    for space in flattened:
        if space not in declared:
            raise LayoutError(
                f"the {name} variant declares the space {space} flattened, "
                f"but prepares nothing there")
    for space in sorted(declared):
        kinds = sorted(kind for kind, where in spaces.items() if where == space)
        if len(kinds) > 1 and space not in flattened:
            raise LayoutError(
                f"the {name} variant prepares {', '.join(kinds)} in the same {space} "
                f"and does not declare it flattened")

    # What the builders really do, which the declaration may describe but not
    # contradict.
    topology = CONSUMER_TOPOLOGY.get(name)
    if topology is None:
        raise LayoutError(f"no builder topology is known for the {name} variant")
    if topology == "one-directory":
        if len(declared) != 1:
            raise LayoutError(
                f"the {name} builder prepares every source in one directory, but this variant "
                f"declares {len(declared)}: {', '.join(sorted(declared))}")
        only = next(iter(declared))
        if only not in flattened:
            raise LayoutError(
                f"the {name} builder prepares every source in one directory, but this variant "
                f"does not declare {only} flattened")
    else:
        for space in sorted(declared):
            kinds = sorted(kind for kind, where in spaces.items() if where == space)
            if len(kinds) > 1:
                raise LayoutError(
                    f"the {name} builder prepares each kind of source in its own directory, "
                    f"but this variant prepares {', '.join(kinds)} in the same {space}")
        if flattened:
            raise LayoutError(
                f"the {name} builder prepares each kind of source in its own directory, "
                f"but this variant declares {', '.join(sorted(flattened))} flattened")


def source_space(settings, entry, name):
    """The directory a builder prepares this source in, before staging.

    Two sources share a name only where they share a directory. The three
    archive variants keep one per kind of source - the binaries, the
    completions, the repository, the generated files - so a completion called
    rmux and a binary called rmux never meet. The RPM builder flattens
    everything into its SOURCES directory, binaries included, so there they do.
    """
    spaces = settings.get("source_spaces")
    kind = entry["source"]["kind"]
    if not spaces or kind not in spaces:
        raise LayoutError(
            f"the {name} variant declares no preparation space for {kind} sources")
    return spaces[kind]


def prepared_identity(settings, entry, space):
    """What names this file inside the space it is prepared in.

    Where the consumer copies everything into one directory, that is the name
    it copies it under, and two sources of that name are one file. Where it
    does not, a file is what it really is: a repository source is read where
    it lives, so two distinct paths are two files even when their last segment
    is the same, and a source prepared under a name of its own is that name.
    """
    if space in settings.get("flattened_source_spaces", []):
        return staged_source_name(entry)
    source = entry["source"]
    if source["kind"] == "repo":
        return source["path"]
    return staged_source_name(entry)


def source_origin(entry):
    """What a prepared file comes from, whatever kind of source it is."""
    source = entry["source"]
    return (source["kind"], source.get("shell"), source.get("path"), source.get("name"))


def describe_origin(origin):
    kind, shell, path, name = origin
    if shell is not None:
        return f"{kind} {shell}"
    return f"{kind} {path or name}"


def validate_variant(descriptor, name):
    """Refuse a variant whose own entries would collide.

    A package path claimed twice, or two entries installing onto the same
    destination, is refused here: before a builder stages it, before a
    manifest is written from it, and so before any installer mutates a prefix
    from it. Roles do not make a collision acceptable; an asset that lands on
    the dispatcher replaces the dispatcher.
    """
    settings = variant(descriptor, name)
    validate_source_spaces(settings, name)
    paths = {}
    destinations = {}
    identifiers = set()
    prepared = {}
    for entry, placement in entries(descriptor, name):
        identifier = entry["id"]
        if not isinstance(identifier, str) or not ENTRY_ID.match(identifier):
            raise LayoutError(f"entry id is not usable: {identifier!r}")
        if identifier in identifiers:
            raise LayoutError(f"two entries share the id {identifier}")
        identifiers.add(identifier)
        # A builder prepares its sources under the names they are declared
        # with, so two entries prepared as one file must come from one place.
        # One file is one name in one preparation directory, compared without
        # case because the same package is prepared on filesystems that keep
        # it and on filesystems that do not. Declaring the same file twice is
        # legitimate - the ConPTY runtime is staged at two destinations from
        # one source - but two different origins under one name would leave
        # the last one holding the file, and the other destination would
        # receive its bytes.
        space = source_space(settings, entry, name)
        prepared_name = prepared_identity(settings, entry, space)
        origin = source_origin(entry)
        key = (space, prepared_name.casefold())
        if key in prepared and prepared[key][0] != origin:
            other, other_origin, other_name = prepared[key][1], prepared[key][0], prepared[key][2]
            raise LayoutError(
                f"two sources prepared in the {space} of {name} are one file: "
                f"{identifier} ({describe_origin(origin)}, {prepared_name}) and "
                f"{other} ({describe_origin(other_origin)}, {other_name})")
        prepared[key] = (origin, identifier, prepared_name)
        canonical_path(placement["path"], f"the package path of {identifier}")
        if placement.get("install") is not None:
            canonical_path(placement["install"], f"the destination of {identifier}")
        for alternative in placement.get("alternatives", []):
            canonical_path(alternative, f"an alternative path of {identifier}")
        claimed = normalised_destination(placement["path"])
        if claimed in paths and paths[claimed][0] != entry["id"]:
            other, other_path = paths[claimed]
            raise LayoutError(
                f"two entries claim the same package path: "
                f"{entry['id']} ({placement['path']}) and {other} ({other_path})")
        paths[claimed] = (entry["id"], placement["path"])
        destination = placement.get("install")
        if destination is None:
            continue
        key = normalised_destination(destination)
        if key in destinations:
            other, other_destination, other_role = destinations[key]
            raise LayoutError(
                f"two entries install to the same destination: "
                f"{entry['id']} ({entry['role']}, {destination}) and "
                f"{other} ({other_role}, {other_destination})")
        destinations[key] = (entry["id"], destination, entry["role"])


def install_pairs(descriptor, name, package_root, prefix):
    """(source, destination, mode, required) for every installed entry."""
    settings = variant(descriptor, name)
    validate_variant(descriptor, name)
    pairs = []
    for entry, placement in entries(descriptor, name):
        destination = placement.get("install")
        if destination is None:
            continue
        occupied = present_path(package_root, placement) if package_root else placement["path"]
        required = not placement.get("install_optional", False)
        if occupied is None:
            if required:
                raise LayoutError(f"missing required package file: {placement['path']}")
            continue
        mode = placement.get("mode")
        if mode is None and settings["modes"]:
            mode = "0755" if entry["role"] == "executable" else "0644"
        pairs.append((
            placement.get("install_order", 50),
            os.path.join(package_root, occupied) if package_root else occupied,
            os.path.join(prefix, destination) if prefix else destination,
            mode or "",
            "required" if required else "optional",
        ))
    # The dispatcher an installer replaces last keeps the order the descriptor
    # gives it; everything else follows in descriptor order.
    return [pair[1:] for pair in sorted(pairs, key=lambda pair: pair[0])]


def same_bytes(left, right):
    """Whether two files hold the same bytes, read without following a link."""
    with open(left, "rb") as one, open(right, "rb") as other:
        while True:
            chunk = one.read(65536)
            if chunk != other.read(65536):
                return False
            if not chunk:
                return True


def check_installed(descriptor, name, package_root, prefix):
    """An installed prefix against the package that was accepted at build.

    Every file the package owns is compared with the file it came from: its
    type, its declared mode and its bytes. Only those destinations are
    examined, so a prefix shared with other packages keeps its own files.
    """
    settings = variant(descriptor, name)
    validate_variant(descriptor, name)
    problems = []
    for entry, placement in entries(descriptor, name):
        destination = placement.get("install")
        if destination is None:
            continue
        occupied = None
        if package_root is not None:
            occupied = present_path(package_root, placement)
            if occupied is None:
                reason = absent_reason(package_root, placement)
                if reason is not None:
                    problems.append(reason)
                    continue
                if placement.get("install_optional"):
                    continue
                problems.append(f"missing required package file: {placement['path']}")
                continue
        installed = Path(prefix) / destination
        try:
            status = installed.lstat()
        except OSError:
            problems.append(f"missing installed file: {destination}")
            continue
        if stat.S_ISLNK(status.st_mode):
            problems.append(f"installed file is a symbolic link: {destination}")
            continue
        if not stat.S_ISREG(status.st_mode):
            problems.append(f"installed file is not a regular file: {destination}")
            continue
        if settings["modes"]:
            expected = placement.get("mode")
            if expected is None:
                expected = "0755" if entry["role"] == "executable" else "0644"
            actual = full_mode(status)
            if actual != expected:
                problems.append(
                    f"installed file mode is {actual}, expected {expected}: {destination}"
                )
        if occupied is not None and not same_bytes(Path(package_root) / occupied, installed):
            problems.append(
                f"installed file differs from the package file it comes from: "
                f"{destination} (from {occupied})"
            )
    if problems:
        raise LayoutError("\n".join(problems))


HEX_DIGEST = re.compile(r"^[0-9a-f]{64}$")


def file_digest(root, relative):
    """The SHA-256 of one owned file of a package or prefix tree."""
    digest = hashlib.sha256()
    with open(Path(root) / relative, "rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


PAYLOAD_TREES = ("package", "prefix")


def payload_binary_roles(descriptor, name, tree="package"):
    """The binary entries of a variant, in descriptor order: id to path.

    A package and a prefix are two topologies of the same entries, and they
    are not interchangeable: windows-archive carries the CLI as rmux.exe in
    the archive and installs it as bin/rmux.exe. The caller says which tree it
    is binding, and this projection answers with that tree's paths only. It
    never looks for one path and falls back to the other: a stray copy left at
    the archive path of an installed prefix must not be what gets verified.
    """
    if tree not in PAYLOAD_TREES:
        raise LayoutError(f"unknown payload tree: {tree} (known: {', '.join(PAYLOAD_TREES)})")
    roles = {}
    for entry, placement in entries(descriptor, name):
        if entry["source"]["kind"] != "binary":
            continue
        if tree == "package":
            roles[entry["id"]] = placement["path"]
            continue
        destination = placement.get("install")
        if destination is None:
            raise LayoutError(
                f"payload provenance: {entry['id']} is not installed by {name}, "
                "so an installed prefix cannot be bound to it")
        roles[entry["id"]] = destination
    return roles


def payload_provenance(descriptor, name, root, declaration, tree="package"):
    """Bind a declared payload provenance to the binaries a tree really holds.

    A package can be assembled, with --skip-build, from binaries the
    assembling repository did not produce. The assembly metadata is then true
    about the assembly and says nothing about where the payload came from.
    This projection is what closes that gap without inventing a field value:
    the declaration names the payload source and, for every binary role of the
    variant, the digest that role's file must carry; the projection refuses
    unless every role is declared once and every declared digest is the digest
    of the file really carried at that role's path in the named tree. What it
    returns is therefore a provenance the tree itself supports, and a builder
    can embed it.

    The variant is validated first, by the same validator its sibling
    projections use: an identity that a manifest, a check or an installer
    would refuse must not be able to receive a positive provenance here.
    """
    validate_variant(descriptor, name)
    for field in ("source", "tree", "configuration"):
        value = declaration.get(field)
        if not isinstance(value, str) or not value.strip():
            raise LayoutError(f"payload provenance: {field} must be a non-empty string")
    declared = declaration.get("files")
    if not isinstance(declared, list) or not declared:
        raise LayoutError("payload provenance: files must be a non-empty list")

    roles = payload_binary_roles(descriptor, name, tree)
    seen = {}
    for item in declared:
        if not isinstance(item, dict):
            raise LayoutError("payload provenance: every file must be an object")
        role = item.get("role")
        digest = item.get("sha256")
        # A role arrives from JSON, where it can be a list or an object. Those
        # are not names, and asking whether one is a known role would raise an
        # unhashable-type error instead of naming the refusal.
        if not isinstance(role, str):
            raise LayoutError(
                f"payload provenance: a role must be a string, not {type(role).__name__}")
        if role not in roles:
            known = ", ".join(sorted(roles))
            raise LayoutError(
                f"payload provenance: unknown binary role: {role} (known: {known})"
            )
        if role in seen:
            raise LayoutError(f"payload provenance: role declared twice: {role}")
        if not isinstance(digest, str) or not HEX_DIGEST.match(digest):
            raise LayoutError(
                f"payload provenance: {role} must declare a lowercase sha256 digest"
            )
        seen[role] = digest

    missing = [role for role in roles if role not in seen]
    if missing:
        raise LayoutError(
            "payload provenance: undeclared binary role: " + ", ".join(sorted(missing))
        )

    files = []
    for role, path in roles.items():
        if not owned_file(root, path):
            raise LayoutError(f"payload provenance: missing packaged binary: {path}")
        actual = file_digest(root, path)
        if actual != seen[role]:
            raise LayoutError(
                f"payload provenance: {path} is {actual}, declared {seen[role]}"
            )
        files.append({"role": role, "path": path, "sha256": actual})

    return {
        "schema": 1,
        "source": declaration["source"],
        "tree": declaration["tree"],
        "configuration": declaration["configuration"],
        "files": files,
    }


def embeddable_field(name, value, indent=2):
    """One JSON member, indented and comma-terminated, ready to interpolate.

    A builder writes its metadata with a here-document, so a projection it
    embeds arrives as text. Emitting the member here keeps the quoting and the
    indentation in one place instead of in every builder.
    """
    body = json.dumps(value, indent=indent, sort_keys=False)
    pad = " " * indent
    lines = body.split("\n")
    return pad + json.dumps(name) + ": " + ("\n" + pad).join(lines) + ","


def tab(rows):
    return "\n".join("\t".join(str(field) for field in row) for row in rows)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--descriptor", default=str(DESCRIPTOR))
    commands = parser.add_subparsers(dest="command", required=True)

    def with_variant(sub):
        sub.add_argument("--variant", required=True)
        return sub

    commands.add_parser("variants")
    identity = with_variant(commands.add_parser("layout-id"))
    identity.add_argument("--legacy", action="store_true",
                          help="the formats this variant still installs, one per line")
    listing = with_variant(commands.add_parser("paths"))
    listing.add_argument("--with-modes", action="store_true")

    plan = with_variant(commands.add_parser("stage-plan"))
    plan.add_argument("--repo")
    plan.add_argument("--binaries")
    plan.add_argument("--completions")
    plan.add_argument("--generated")
    plan.add_argument("--conpty")
    plan.add_argument("--only-sources", help="comma-separated source kinds to emit")
    plan.add_argument("--staged-names", action="store_true",
                      help="emit the builder-side source file name of each entry")

    check = with_variant(commands.add_parser("check-tree"))
    check.add_argument("--root", required=True)
    check.add_argument("--skip", default="")

    generated = with_variant(commands.add_parser("generated-modes"))
    generated.add_argument("--package-root")

    manifest = with_variant(commands.add_parser("manifest"))
    manifest.add_argument("--package-root")

    install = with_variant(commands.add_parser("install-plan"))
    install.add_argument("--package-root")
    install.add_argument("--prefix", default="")

    uninstall = with_variant(commands.add_parser("uninstall-plan"))
    uninstall.add_argument("--prefix", default="")

    installed = with_variant(commands.add_parser("check-installed"))
    installed.add_argument("--prefix", required=True)
    installed.add_argument("--package-root")

    commands.add_parser("rpm-files")
    spec = commands.add_parser("rpm-spec-install")
    spec.add_argument("--binaries-dir", required=True)
    spec.add_argument("--sourcedir", default="%{_sourcedir}")
    commands.add_parser("homebrew-install")
    nested = commands.add_parser("nested-exe")
    nested.add_argument("--package-name", required=True)
    nested.add_argument("--separator", default="\\")
    nested.add_argument("--entry", help="entry id (default: the WinGet nested entry)")
    alias = commands.add_parser("command-alias")
    alias.add_argument("--consumer", default="winget")
    shims = commands.add_parser("chocolatey-shims")
    shims.add_argument("--package-name", default="")
    shims.add_argument("--separator", default="\\")
    commands.add_parser("chocolatey-private")
    ignores = commands.add_parser("chocolatey-ignores")
    ignores.add_argument("--separator", default="\\")
    provenance = with_variant(commands.add_parser("payload-provenance"))
    provenance.add_argument("--package-root",
                            help="the staged or extracted package to bind, at its package paths")
    provenance.add_argument("--prefix",
                            help="the installed prefix to bind, at its installed destinations")
    provenance.add_argument("--declaration", required=True,
                            help="the declared payload provenance, as JSON")
    provenance.add_argument("--field", action="store_true",
                            help="emit one indented JSON member a builder can embed")

    sources = with_variant(commands.add_parser("source-names"))
    sources.add_argument("--kind", required=True,
                         help="source kind: binary, repo, completion, generated, conpty")
    sources.add_argument("--with-origin", action="store_true",
                         help="emit the generator key of each source beside its name")

    arguments = parser.parse_args(argv)

    # Every projection is read as lines, and a shell that reads them with
    # `read` keeps whatever ends the line. On Windows the default text mode
    # would end it with CR LF and leave the CR inside the last field, which
    # then travels into a generated script. The projections end with LF on
    # every host.
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(newline="\n")

    descriptor = load(arguments.descriptor)

    if arguments.command == "payload-provenance":
        # The two topologies are named, never guessed: a package is bound at
        # its package paths and a prefix at its installed destinations.
        if (arguments.package_root is None) == (arguments.prefix is None):
            raise LayoutError(
                "payload provenance: name exactly one tree, --package-root or --prefix")
        tree = "package" if arguments.package_root is not None else "prefix"
        root = arguments.package_root if tree == "package" else arguments.prefix
        try:
            declaration = json.loads(Path(arguments.declaration).read_text(encoding="utf-8"))
        except OSError as error:
            raise LayoutError(f"payload provenance: cannot read {arguments.declaration}: {error}") from None
        except json.JSONDecodeError as error:
            raise LayoutError(f"payload provenance: {arguments.declaration} is not JSON: {error}") from None
        if not isinstance(declaration, dict):
            raise LayoutError("payload provenance: the declaration must be a JSON object")
        projected = payload_provenance(descriptor, arguments.variant, root, declaration, tree)
        if arguments.field:
            print(embeddable_field("payload_provenance", projected))
        else:
            print(json.dumps(projected, indent=2, sort_keys=False))
        return 0

    if arguments.command == "variants":
        print("\n".join(sorted(descriptor["variants"])))
        return 0

    if arguments.command == "layout-id":
        settings = variant(descriptor, arguments.variant)
        if arguments.legacy:
            print("\n".join(settings.get("legacy_package_layouts", [])))
        else:
            print(settings["package_layout"])
        return 0

    if arguments.command == "paths":
        rows = []
        for entry, placement in entries(descriptor, arguments.variant):
            if arguments.with_modes:
                rows.append((placement["path"], placement.get("mode", ""), entry["id"]))
            else:
                rows.append((placement["path"],))
        print(tab(rows))
        return 0

    if arguments.command == "stage-plan":
        settings = variant(descriptor, arguments.variant)
        validate_variant(descriptor, arguments.variant)
        roots = {
            "repo": arguments.repo,
            "binaries": arguments.binaries,
            "completions": arguments.completions,
            "generated": arguments.generated,
            "conpty": arguments.conpty,
        }
        wanted = None
        if arguments.only_sources:
            wanted = {kind.strip() for kind in arguments.only_sources.split(",")}
        rows = []
        for entry, placement in entries(descriptor, arguments.variant):
            kind = entry["source"]["kind"]
            if wanted is not None and kind not in wanted:
                continue
            source = source_path(entry, placement, roots, settings["executable_suffix"])
            if source is None:
                continue
            mode = placement.get("mode")
            if mode is None and settings["modes"]:
                mode = "0755" if entry["role"] == "executable" else "0644"
            row = [source, placement["path"], mode or "", placement.get("transform", "copy")]
            if arguments.staged_names:
                row.append(staged_source_name(entry))
            rows.append(tuple(row))
        print(tab(rows))
        return 0

    if arguments.command == "check-tree":
        validate_variant(descriptor, arguments.variant)
        skip = {value for value in arguments.skip.split(",") if value}
        check_tree(descriptor, arguments.variant, arguments.root, skip)
        return 0

    if arguments.command == "generated-modes":
        settings = variant(descriptor, arguments.variant)
        rows = []
        for entry, placement in entries(descriptor, arguments.variant):
            if entry["source"]["kind"] != "generated" or not settings["modes"]:
                continue
            occupied = placement["path"]
            if arguments.package_root:
                found = present_path(arguments.package_root, placement)
                if found is not None:
                    occupied = found
            mode = placement.get("mode") or ("0755" if entry["role"] == "executable" else "0644")
            rows.append((occupied, mode))
        print(tab(rows))
        return 0

    if arguments.command == "manifest":
        settings = variant(descriptor, arguments.variant)
        validate_variant(descriptor, arguments.variant)
        rows = []
        for entry, placement in entries(descriptor, arguments.variant):
            occupied = placement["path"]
            if arguments.package_root:
                found = present_path(arguments.package_root, placement)
                if found is not None:
                    occupied = found
            mode = placement.get("mode")
            if mode is None and settings["modes"]:
                mode = "0755" if entry["role"] == "executable" else "0644"
            rows.append((
                placement.get("install_order", 50),
                occupied,
                mode or "-",
                placement.get("install") or "-",
                "optional" if placement.get("install_optional") else "required",
                entry["id"],
                entry["role"],
            ))
        print(tab(row[1:] for row in sorted(rows, key=lambda row: row[0])))
        return 0

    if arguments.command == "install-plan":
        print(tab(install_pairs(descriptor, arguments.variant,
                                arguments.package_root, arguments.prefix)))
        return 0

    if arguments.command == "uninstall-plan":
        validate_variant(descriptor, arguments.variant)
        rows = []
        for _entry, placement in entries(descriptor, arguments.variant):
            destination = placement.get("install")
            if destination is None:
                continue
            rows.append((os.path.join(arguments.prefix, destination)
                         if arguments.prefix else destination,))
        print(tab(rows))
        return 0

    if arguments.command == "check-installed":
        check_installed(descriptor, arguments.variant, arguments.package_root, arguments.prefix)
        return 0

    if arguments.command == "rpm-spec-install":
        validate_variant(descriptor, "rpm")
        rows = []
        for entry, placement in entries(descriptor, "rpm"):
            source = entry["source"]
            if source["kind"] == "binary":
                origin = os.path.join(arguments.binaries_dir, source["name"])
            else:
                origin = os.path.join(arguments.sourcedir, staged_source_name(entry))
            rows.append((
                f"install -Dm{placement['mode']} \"{origin}\" %{{buildroot}}/{placement['path']}",
            ))
        print(tab(rows))
        return 0

    if arguments.command == "rpm-files":
        settings = variant(descriptor, "rpm")
        validate_variant(descriptor, "rpm")
        # RPM classifies documentation and licences itself, and mechanisms
        # outside the file list depend on those flags. Paths and modes alone
        # do not carry them, so the descriptor names the class of an entry and
        # this variant maps each class to the directive that expresses it.
        attributes = settings.get("content_attributes", {})
        rows = []
        for entry, placement in entries(descriptor, "rpm"):
            path = "/" + placement["path"]
            if placement.get("alternatives"):
                path += "*"
            owned = f"%attr({placement['mode']},root,root) {path}"
            content_class = entry.get("content_class")
            if content_class is not None:
                if content_class not in attributes:
                    raise LayoutError(
                        f"the rpm variant declares no attribute for the "
                        f"{content_class} class: {entry['id']}")
                owned = f"{attributes[content_class]} {owned}"
            rows.append((owned,))
        print(tab(rows))
        return 0

    if arguments.command == "homebrew-install":
        mapping = downstream(descriptor, "homebrew")
        rows = []
        for item in mapping["install_map"]:
            # The formula installs the placement the descriptor gives the
            # entry. Moving a file in the descriptor moves it in the formula,
            # instead of leaving the old path behind in a literal.
            _entry, placement = placement_of(descriptor, mapping["from_variant"], item["entry"])
            target = item["into"]
            subdirectory = item.get("subdirectory")
            if subdirectory is not None:
                target = f'({target}/"{subdirectory}")'
            rows.append((f'{target}.install "{placement["path"]}"',))
        print(tab(rows))
        return 0

    if arguments.command == "nested-exe":
        mapping = downstream(descriptor, "winget")
        wanted = arguments.entry or mapping["nested_entry"]
        _entry, placement = placement_of(descriptor, mapping["from_variant"], wanted)
        path = placement["path"].replace("/", arguments.separator)
        print(f"{arguments.package_name}{arguments.separator}{path}")
        return 0

    if arguments.command == "command-alias":
        mapping = downstream(descriptor, arguments.consumer)
        print(mapping["portable_command_alias"])
        return 0

    if arguments.command == "chocolatey-shims":
        mapping = downstream(descriptor, "chocolatey")
        variant_name = mapping["from_variant"]
        shimmed = set()
        rows = []
        for shim in mapping["shims"]:
            entry, placement = placement_of(descriptor, variant_name, shim["entry"])
            if entry["role"] != "executable":
                raise LayoutError(
                    f"chocolatey shims a file that is not an executable: {shim['entry']}")
            shimmed.add(shim["entry"])
            path = placement["path"].replace("/", arguments.separator)
            rows.append((shim["command"],
                         f"{arguments.package_name}{arguments.separator}{path}"
                         if arguments.package_name else path))
        # What the package carries but never shims is named too, so the list
        # of commands is a decision and not an omission.
        for identifier in mapping.get("private_entries", []):
            if identifier in shimmed:
                raise LayoutError(f"chocolatey declares {identifier} both shimmed and private")
            placement_of(descriptor, variant_name, identifier)
        print(tab(rows))
        return 0

    if arguments.command == "source-names":
        validate_variant(descriptor, arguments.variant)
        # The file names a builder must have on hand before it stages this
        # kind of source, in descriptor order. A builder that generates or
        # caches them reads this list instead of repeating one of its own.
        rows = []
        seen = set()
        for entry, _placement in entries(descriptor, arguments.variant):
            source = entry["source"]
            if source["kind"] != arguments.kind:
                continue
            name = staged_source_name(entry)
            # One file from one place is listed once; two places under one
            # name never reach here, they are refused above.
            fingerprint = (name, source_origin(entry))
            if fingerprint in seen:
                continue
            seen.add(fingerprint)
            if arguments.with_origin:
                # The key the generator reports its own outputs under. It is
                # the one thing the two sides share: the generator names the
                # files it wrote, this descriptor names what the package calls
                # them, and the preparation joins them here.
                origin = source.get("shell")
                if origin is None:
                    raise LayoutError(
                        f"the {arguments.kind} source of {entry['id']} declares no origin")
                rows.append((origin, name))
            else:
                rows.append((name,))
        if not rows:
            raise LayoutError(
                f"the {arguments.variant} variant declares no {arguments.kind} source")
        print(tab(rows))
        return 0

    if arguments.command == "chocolatey-private":
        mapping = downstream(descriptor, "chocolatey")
        rows = []
        for identifier in mapping.get("private_entries", []):
            _entry, placement = placement_of(descriptor, mapping["from_variant"], identifier)
            rows.append((identifier, placement["path"]))
        print(tab(rows))
        return 0

    if arguments.command == "chocolatey-ignores":
        # Chocolatey's automatic pass shims every executable a package carries
        # unless a <file>.ignore sits beside it, and it names each command
        # after the file's own basename. A declared command is free to differ
        # from that basename, so leaving an explicitly shimmed file out of this
        # list would let the automatic pass add its basename as a third
        # command beside the two the descriptor declares. Every packaged
        # executable is reported here, and the command surface is then decided
        # only by the explicit Install-BinFile list. An executable that is
        # neither given a command nor declared private is refused instead of
        # travelling unclassified.
        mapping = downstream(descriptor, "chocolatey")
        variant_name = mapping["from_variant"]
        shimmed = set()
        for shim in mapping["shims"]:
            _entry, placement = placement_of(descriptor, variant_name, shim["entry"])
            shimmed.add(placement["path"])
        private = set()
        for identifier in mapping.get("private_entries", []):
            _entry, placement = placement_of(descriptor, variant_name, identifier)
            private.add(placement["path"])
        rows = []
        unclassified = []
        for entry, placement in entries(descriptor, variant_name):
            path = placement["path"]
            if not path.lower().endswith(".exe"):
                continue
            if path in shimmed or path in private:
                rows.append((path.replace("/", arguments.separator),))
            else:
                unclassified.append(entry["id"])
        if unclassified:
            raise LayoutError(
                "chocolatey neither shims nor declares private: "
                + ", ".join(sorted(unclassified)))
        print(tab(rows))
        return 0

    raise LayoutError(f"unknown command: {arguments.command}")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except LayoutError as error:
        print(f"error: package layout: {error}", file=sys.stderr)
        sys.exit(2)
