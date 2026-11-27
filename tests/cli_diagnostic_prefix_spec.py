"""Focused Linux CLI checks; pass a packaged rmux path and an owned fixture directory."""
import json
import os
from pathlib import Path
import subprocess
import sys


def check(binary, root):
    root.mkdir(parents=True, exist_ok=True)
    root = root.resolve()
    binary = binary.absolute()
    tmux = binary.with_name("tmux")
    os.link(binary, tmux)
    config = root / "config"
    config.write_text("# Nonexecuting diagnostic fixture.\n")
    socket = root / "absent.sock"
    env = {"HOME": str(root), "PATH": "/usr/bin:/bin", "TERM": "dumb",
           "XDG_CONFIG_HOME": str(root), "TMUX_TMPDIR": str(root),
           "RMUX_TMPDIR": str(root)}
    count = 0

    def run(executable, args):
        nonlocal count
        count += 1
        result = subprocess.run([executable, "-N", "-S", socket, *args],
                                cwd=root, env=env, capture_output=True, timeout=10)
        return result.returncode, result.stdout, result.stderr

    for executable in [binary, tmux]:
        for command in ["diagnose", "capabilities", "doctor", "setup"]:
            expected = run(executable, [command, "--help"])
            assert expected[0] == 0 and b"usage:" in expected[1], expected
            for flag in "LSfTc":
                for value in ["name", "", "--", "--help", "-V", command, b"opaque-\xff"]:
                    value = os.fsencode(value)
                    for prefix in [[f"-u{flag}", value], [b"-u" + flag.encode() + value]]:
                        if not value and len(prefix) == 1:
                            continue
                        for separator in [[], ["--"]]:
                            actual = run(executable, [*prefix, *separator, command, "--help"])
                            assert actual == expected, (command, prefix, actual, expected)
            for tail in [["--bad", "--help"], ["--help", "--bad"], ["--"]]:
                assert run(executable, ["-uLname", command, *tail]) == run(executable, [command, *tail])
            for early in ["-V", "-h", "-uxV", "--help", "--version"]:
                assert run(executable, [early, "-uLname", command, "--help"]) == run(executable, [early])
        plain = ["-2", "-u", "-f", config, "-f", config, "-T", "RGB, clipboard,RGB",
                 "-T", "256", "diagnose", "--json"]
        grouped = ["-q2uf" + str(config), "-uf" + str(config), "-uTRGB, clipboard,RGB",
                   "-T256", "diagnose", "--json"]
        expected = run(executable, plain)
        actual = run(executable, grouped)
        assert expected[0] == 0 and actual == expected, (actual, expected)
        report = json.loads(actual[1])
        assert report["config"]["paths"] == ["~/config", "~/config"], report
        assert report["capabilities"]["terminal_features"] == ["RGB", "clipboard", "256"]
        assert not socket.exists()
    assert config.read_text() == "# Nonexecuting diagnostic fixture.\n"
    assert {path.name for path in root.iterdir()} == {"config"}
    print(f"{binary}: {count} focused subprocess checks passed")


if __name__ == "__main__":
    check(Path(sys.argv[1]), Path(sys.argv[2]))
