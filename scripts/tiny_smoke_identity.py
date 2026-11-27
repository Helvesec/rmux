"""Keep the tiny smoke's installed layout inside its qualified command target."""
from pathlib import Path
import subprocess
import sys
import tempfile

import candidate_identity as identity


IMAGES = ('bin/rmux', 'libexec/rmux/rmux', 'bin/rmux-daemon')


def package_path(layout, target):
    path = Path(layout).absolute()
    # Check existing parents as well as the leaf before install can write through
    # a redirected bin/ or libexec/. The verifier repeats containment on reuse.
    for selected in (path, *(path / name for name in IMAGES)):
        if not selected.resolve().is_relative_to(target):
            raise identity.IdentityError('package layout outside candidate target')
    if path.resolve() == target:
        raise identity.IdentityError('package layout must be below candidate target')
    return path


def main():
    try:
        mode, build_dir, layout = sys.argv[1:]
        identity.validate_context()
        source, head, target = identity.context()
        if Path(build_dir).absolute() != target:
            raise identity.IdentityError('package build target differs from candidate target')
        if mode == 'layout':
            if not layout:
                layout = tempfile.mkdtemp(prefix='tiny-release-layout.', dir=target)
            print(package_path(layout, target))
        elif mode == 'verify':
            path = package_path(layout, target)
            # Use completed compiler observations, including retained objects and
            # their linked inputs. A marker or equal copy hash alone is not proof.
            for name in IMAGES:
                identity.verify_built(path / name, source, head, target)
        else:
            raise identity.IdentityError('unknown tiny smoke identity action')
        return 0
    except (identity.IdentityError, OSError, subprocess.CalledProcessError) as error:
        print(f'candidate package identity FAIL: {error}', file=sys.stderr)
        return 86


if __name__ == '__main__':
    sys.exit(main())
