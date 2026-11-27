"""Pair build-derived notices with their immutable canonical package assets."""
from pathlib import Path

SUFFIX = ".NOTICE.txt"
PACKAGE_ROLES = {"archive", "debian", "rpm"}
NOTICE_ROLES = {role + "-notice" for role in PACKAGE_ROLES}


def package_files(files):
    """Accept historical records; require a complete paired set when extended."""
    if any(not isinstance(item, dict) or not isinstance(item.get("role"), str)
           or not isinstance(item.get("path"), str)
           for item in files):
        raise ValueError("canonical asset must have a string role")
    notices = [item for item in files if item["role"] in NOTICE_ROLES]
    packages = [item for item in files if item["role"] not in NOTICE_ROLES]
    if not notices:
        return packages
    expected = {item["path"] + SUFFIX: item["role"] + "-notice"
                for item in packages if item["role"] in PACKAGE_ROLES}
    actual = {item["path"]: item["role"] for item in notices}
    if actual != expected or len(notices) != len(expected):
        raise ValueError("notice assets must pair with every canonical package exactly once")
    return packages


def verify_contents(directory: Path, files):
    packages = {item["path"]: item for item in package_files(files)}
    for item in files:
        if item["role"] not in NOTICE_ROLES:
            continue
        name = item["path"][:-len(SUFFIX)]
        package = packages[name]
        expected = (f"Package: {Path(name).name}\nSHA256: {package['sha256']}\n\n"
                    "RMUX third-party notices\n")
        try:
            # Preserve line endings: the notice digest covers its original bytes.
            contents = (directory / item["path"]).read_bytes().decode("utf-8")
        except (OSError, UnicodeError) as error:
            raise ValueError("notice asset must be readable UTF-8") from error
        if not contents.startswith(expected) or not contents[len(expected):].strip():
            raise ValueError(f"notice does not bind its canonical package: {item['path']}")
