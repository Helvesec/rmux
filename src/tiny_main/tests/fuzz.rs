use super::*;

#[test]
fn display_message_tiny_parser_fuzz_stays_on_allowlist() {
    let pool = [
        "-p",
        "-t",
        "bench",
        "bench:0",
        "bench:0.0",
        "%1",
        "-F",
        "#{pane_id}",
        "#{session_name}:#{window_index}.#{pane_index}",
        "literal",
        "-c",
        "client",
        "-d",
        "10",
        "-a",
        "-I",
        "-l",
        "-N",
        "-v",
        "--json",
        "--",
        ";",
        "-hyphen-message",
    ];
    let mut rng = FuzzRng::new(0xD15F_1A7E_2026_0700);

    for _ in 0..10_000 {
        let args = fuzz_args(&mut rng, &pool, 8);
        assert_eq!(
            parse_display_message(&args).is_some(),
            display_message_should_use_tiny(&args),
            "unexpected display-message tiny classification for {args:?}"
        );
    }
}

#[test]
fn send_keys_tiny_parser_fuzz_stays_on_allowlist() {
    let pool = [
        "-t",
        "bench",
        "bench:0",
        "bench:0.0",
        "%1",
        "true",
        "Enter",
        "C-c",
        "--",
        "-literal-key",
        "-F",
        "-H",
        "-l",
        "-K",
        "-M",
        "-p",
        "-R",
        "-X",
        "-N",
        "2",
        "-c",
        "client",
        ";",
    ];
    let mut rng = FuzzRng::new(0x5EED_5EAD_2026_0700);

    for _ in 0..10_000 {
        let args = fuzz_args(&mut rng, &pool, 8);
        assert_eq!(
            parse_send_keys(&args).is_some(),
            send_keys_should_use_tiny(&args),
            "unexpected send-keys tiny classification for {args:?}"
        );
    }
}

#[test]
fn source_file_tiny_parser_fuzz_stays_on_allowlist() {
    let pool = [
        "/tmp/rmux-a.conf",
        "/tmp/rmux-b.conf",
        "relative.conf",
        "/tmp/#{session_name}.conf",
        "-",
        "-q",
        "-F",
        "-n",
        "-v",
        "-t",
        "bench:0.0",
        "--",
        "-dash.conf",
        ";",
    ];
    let mut rng = FuzzRng::new(0x50C0_2026_0700);

    for _ in 0..10_000 {
        let args = fuzz_args(&mut rng, &pool, 8);
        assert_eq!(
            parse_source_file(&args).is_some(),
            source_file_should_use_tiny(&args),
            "unexpected source-file tiny classification for {args:?}"
        );
    }
}
