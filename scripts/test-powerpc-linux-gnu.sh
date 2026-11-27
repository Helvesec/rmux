#!/usr/bin/env bash
# Cross-compile in Docker; execute PowerPC test binaries with qemu-user.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ "${1:-}" == --in-container ]]; then
    target=powerpc-unknown-linux-gnu
    # The giant server harness wraps PPC32 GOT offsets at opt-level=0.
    # Keep the workaround scoped to this package's test profile.
    cargo() {
        command cargo --config 'profile.test.package.rmux-server.opt-level=1' "$@"
    }
    rustc -Vv
    qemu-ppc --version
    target_cfg="$(rustc --print cfg --target "$target")"
    printf '%s\n' "$target_cfg"
    if [[ "$target_cfg" == *'target_has_atomic="64"'* ]]; then
        echo 'error: this gate requires a target without native 64-bit atomics' >&2
        exit 1
    fi

    # Include optional features, tests and examples when checking the full tree.
    cargo check --locked --workspace --all-targets --all-features --target "$target"
    cargo check --locked --no-default-features --bins --target "$target"
    cargo test --locked --workspace --all-targets --all-features --target "$target" --no-run
    cargo build --locked --bins --target "$target"

    # These suites exercise revisions, session ordering, SDK IDs, attach epochs,
    # output cursors and web leases without needing binfmt_misc or a PPC kernel.
    run_filtered_tests() {
        local package="$1" filter="$2" listing count
        shift 2
        listing="$(cargo test --locked --target "$target" -p "$package" --lib "$filter" -- --list "$@")"
        count="$(awk '/: test$/ { n += 1 } END { print n + 0 }' <<< "$listing")"
        if (( count == 0 )); then
            echo "error: no tests selected for $package $filter" >&2
            exit 1
        fi
        printf 'cargo-filter-nonempty=ok selected=%s package=%s filter=%s\n' "$count" "$package" "$filter"
        cargo test --locked --target "$target" -p "$package" --lib "$filter" -- "$@"
    }
    cargo test --locked --target "$target" -p rmux-core --lib
    run_filtered_tests rmux-sdk transport::
    run_filtered_tests rmux-client attach::screen::
    # This module re-execs the PPC test binary through a PTY. qemu-user's Cargo
    # runner does not cover child execs; keep binfmt_misc and host setup untouched.
    echo 'Skipping 5 native PTY self-exec tests: rename_admission_native_tests (requires target child-exec support)'
    run_filtered_tests rmux-server pane_io:: --skip pane_io::reader::rename_admission_native_tests::
    run_filtered_tests rmux-server control::pane_event_delivery::
    run_filtered_tests rmux-server web::leases::
    run_filtered_tests rmux-server web::tests::
    run_filtered_tests rmux-server web::outbound::tests::
    qemu-ppc -L /usr/powerpc-linux-gnu target/"$target"/debug/rmux --version
    exit 0
fi

if [[ "$#" != 0 ]]; then
    echo 'usage: scripts/test-powerpc-linux-gnu.sh' >&2
    exit 2
fi

rust_version="$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)"
[[ -n "$rust_version" ]] || { echo 'error: missing pinned Rust version' >&2; exit 1; }
image="rmux-powerpc-linux-gnu:$rust_version"
owner=rmux-powerpc-linux-gnu
if docker image inspect "$image" >/dev/null 2>&1; then
    label="$(docker image inspect --format '{{index .Config.Labels "org.rmux.gate"}}' "$image")"
    [[ "$label" == "$owner" ]] || { echo "error: image $image belongs to another task" >&2; exit 1; }
fi
# The Dockerfile is the entire build context: do not send target/ or local files.
docker build --label "org.rmux.gate=$owner" --build-arg "RUST_VERSION=$rust_version" \
    -t "$image" - < scripts/docker/powerpc-linux-gnu.Dockerfile
mkdir -p target target-audit/powerpc/cargo-home
docker run --rm --pull=never --cpus=2 --ulimit core=0 --label "org.rmux.gate=$owner" \
    --user "$(id -u):$(id -g)" --mount "type=bind,source=$repo_root,target=/work" \
    "$image" bash scripts/test-powerpc-linux-gnu.sh --in-container
