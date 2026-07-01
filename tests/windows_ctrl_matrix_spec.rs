#[test]
fn windows_ctrl_matrix_script_keeps_direct_attach_send_keys_axes() {
    let script = include_str!("../scripts/windows_ctrl_matrix.ps1");
    for required in [
        "function Invoke-DirectCase",
        "function Invoke-AttachCase",
        "function Invoke-SendKeysCase",
        "Direct natif",
        "RMUX attach",
        "RMUX send-keys",
        "SendKeys-ControlTokens",
        "StaticMatrixSpec",
        "\"Ctrl-C\" { return @(\"C-c\", \"Enter\") }",
        "\"Ctrl-Z\" { return @(\"C-z\", \"Enter\") }",
        "Windows Terminal",
        "WezTerm",
        "Alacritty",
        "PortableSmokeOnly",
        "Ctrl-C",
        "Ctrl-D",
        "Ctrl-A",
        "Ctrl-Z",
        "Ctrl-H",
        "Esc",
        "python sleep",
        "python descendant sleep",
        "python stdin",
        "line idle",
        "wsl python sleep",
        "wsl python stdin",
        "powershell.exe",
        "wsl-bash",
        "git-bash",
        "timeout",
        "ping",
        "fzf",
    ] {
        assert!(
            script.contains(required),
            "Windows Ctrl matrix script lost required axis/snippet {required:?}"
        );
    }

    assert!(
        script.contains(
            "$Direct.Returned -eq $Attach.Returned -and $Direct.Returned -eq $Send.Returned"
        ),
        "Windows Ctrl matrix must continue comparing native, attach, and send-keys outcomes"
    );

    assert!(
        script.contains("$Results.Count -eq 0")
            && script.contains("Where-Object { $_.Verdict -eq \"NO GO\" }")
            && script.contains("Windows Ctrl matrix found"),
        "Windows Ctrl matrix must fail closed on empty or NO GO results"
    );
}
