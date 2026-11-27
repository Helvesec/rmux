"""RPM spec projections, including optional generated package sources."""
import os


def source_name(entry):
    source = entry["source"]
    return os.path.basename(source["path"]) if source["kind"] == "repo" else source["name"]


def optional_block(entry, placement, lines):
    if not placement.get("optional"):
        return lines
    origin = "%{_sourcedir}/" + source_name(entry)
    return [f'%if %(test -f "{origin}" && echo 1 || echo 0)', *lines, "%endif"]


def spec_install(entries, binaries, sourcedir):
    rows = []
    for entry, placement in entries:
        root = binaries if entry["source"]["kind"] == "binary" else sourcedir
        origin = os.path.join(root, source_name(entry))
        line = f'install -Dm{placement["mode"]} "{origin}" %{{buildroot}}/{placement["path"]}'
        rows.extend(optional_block(entry, placement, [line]))
    return rows


def file_list(entries, attributes, error):
    rows = []
    for entry, placement in entries:
        path = "/" + placement["path"]
        if placement.get("alternatives"):
            path += "*"
        owned = f'%attr({placement["mode"]},root,root) {path}'
        content_class = entry.get("content_class")
        if content_class is not None:
            if content_class not in attributes:
                raise error(f"the rpm variant declares no attribute for the "
                            f"{content_class} class: {entry['id']}")
            owned = f"{attributes[content_class]} {owned}"
        rows.extend(optional_block(entry, placement, [owned]))
    return rows
