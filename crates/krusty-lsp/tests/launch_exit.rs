use std::process::Command;

#[test]
fn help_exits_zero_and_writes_only_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_krusty-lsp"))
        .arg("--help")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "help must not write a startup error"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Usage: krusty-lsp [options]\n"));
    assert!(stdout.contains("--system-path"));
    assert!(stdout.contains("stdio launch subset"));
}

#[test]
fn an_invalid_option_exits_two_and_writes_only_stderr() {
    let output = Command::new(env!("CARGO_BIN_EXE_krusty-lsp"))
        .arg("--not-a-real-option")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "a startup error must not write help to stdout"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "krusty-lsp: unsupported option '--not-a-real-option'\n"
    );
}
