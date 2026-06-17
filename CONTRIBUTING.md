# Contributing to rmux

## How to contribute

Report bugs and feature requests through GitHub issues:

https://github.com/Helvesec/rmux/issues

Submit code changes as pull requests against `main`.

## Requirements for acceptance

A change should be focused, reviewable, and include a clear commit message.

Before opening a pull request, run:

```sh
cargo build --workspace
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

New behavior should include tests. Bug fixes should include a regression test when practical.

Changes that affect public behavior should update the relevant README or documentation.

Do not commit credentials, private machine paths, local logs, or generated artifacts unless they are intentionally part of the repository.

## Developer Certificate of Origin

By contributing to RMUX, you certify that you have the right to submit the work
under the project license. See [DCO](DCO).

Pull requests for code or documentation must certify this with a `Signed-off-by:`
line:

```text
Signed-off-by: Your Name <you@example.com>
```

## Platform changes

For Linux, macOS, Windows, terminal, PTY, or shell compatibility changes, mention which platform was tested in the pull request.

## Security issues

Do not open a public issue for security problems. See [SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contribution is licensed under `MIT OR Apache-2.0`, matching the project license.
