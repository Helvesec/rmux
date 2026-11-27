#!/usr/bin/env python3
"""Select versioned layouts without changing the pinned historical projector."""
import argparse
import json
from pathlib import Path
import sys
import tempfile

import layout
from layout_profiles import for_package, with_notices
from layout_rpm import file_list, spec_install


def rpm_projection(descriptor, arguments):
    parser = argparse.ArgumentParser(prog="layout " + arguments[0])
    if arguments[0] == "rpm-spec-install":
        parser.add_argument("--binaries-dir", required=True)
        parser.add_argument("--sourcedir", default="%{_sourcedir}")
    options = parser.parse_args(arguments[1:])
    layout.validate_variant(descriptor, "rpm")
    entries = list(layout.entries(descriptor, "rpm"))
    if arguments[0] == "rpm-spec-install":
        rows = spec_install(entries, options.binaries_dir, options.sourcedir)
    else:
        attributes = layout.variant(descriptor, "rpm").get("content_attributes", {})
        rows = file_list(entries, attributes, layout.LayoutError)
    print("\n".join(rows))
    return 0


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--descriptor", default=str(layout.DESCRIPTOR))
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--with-notices", action="store_true")
    selection.add_argument("--for-package", type=Path)
    options, arguments = parser.parse_known_args(argv)
    if not options.with_notices and options.for_package is None:
        return layout.main(argv)
    descriptor = layout.load(options.descriptor)
    descriptor = (with_notices(descriptor) if options.with_notices else
                  for_package(descriptor, options.for_package))
    if arguments and arguments[0] in {"rpm-spec-install", "rpm-files"}:
        return rpm_projection(descriptor, arguments)
    # All other parsing, validation and projections remain in the original
    # projector. Its descriptor argument is already the supported extension
    # boundary, so no global state or historical policy pins are rewritten.
    with tempfile.TemporaryDirectory(prefix="rmux-layout-") as temporary:
        path = Path(temporary) / "layout.json"
        path.write_text(json.dumps(descriptor), encoding="utf-8")
        return layout.main(["--descriptor", str(path), *arguments])


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (layout.LayoutError, OSError, ValueError) as error:
        print(f"error: package layout: {error}", file=sys.stderr)
        raise SystemExit(2)
