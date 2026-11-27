# Run all archive exclusions in one identity context and private Cargo target.
# The caller must enter through candidate_identity.py with -- pwsh -File ... .
$ErrorActionPreference = "Stop"
if (-not $env:RMUX_IDENTITY_PYTHON -or -not $env:RMUX_IDENTITY_SCRIPT) {
    throw "candidate identity context is required"
}
& $env:RMUX_IDENTITY_PYTHON $env:RMUX_IDENTITY_SCRIPT validate-gate
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo test --locked -p rmux --test candidate_identity_children -- --test-threads=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo test --locked -p rmux-server --lib bracketed_paste_final_sink -- --test-threads=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo test --locked -p rmux-server --lib handler::pane_command_tests::respawn_pane_rejects_an_interior_nul_and_the_same_child_keeps_reading -- --exact --test-threads=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo test --locked -p rmux-server --lib test_shell::final_sink::windows_byte_child::tests:: -- --test-threads=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
