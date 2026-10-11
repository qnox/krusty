use std::process::Command;

#[test]
fn help_exits_zero_and_writes_only_stdout() {
    const HELP: &str = "\
Usage: krusty-lsp [options]

  --stdio
      Read and write Language Server Protocol messages on standard input and output.
  --socket <host:port>
      Accepted together with --stdio. Socket transport is not supported on its own.
  --client
      Connect out to a socket. Not supported, and it conflicts with --stdio.
  --multi-client
      Accept another client after disconnect. Not supported, and it conflicts with --stdio and --client.
  --system-path <path>
      Kotlin LSP system directory. Krusty's dependency cache is <path>/krusty/deps.
      This is the stdio launch subset: socket, client, and multiclient modes are not
      implemented, and with no transport flag the server still speaks stdio.
  --log-level <LEVEL>
      TRACE, DEBUG, INFO, WARNING, ERROR, OFF, or ALL. Case-sensitive.
      Accepted and validated for launch compatibility. It does not configure a logger.
      KOTLIN_LSP_LOG_LEVEL supplies the value when the option is absent.
  --log-category <category:LEVEL>
      One category and level. Repeat the option for several categories.
      Accepted and validated for launch compatibility. It does not configure a logger.
      KOTLIN_LSP_LOG_CATEGORIES supplies one category:LEVEL when the option is absent.
  -cp, -classpath, -class-path <path>
  -jdk-home <path>
  -no-jdk
  -deps-cache-dir <path>
  -deps-cache-max-age-days <days>
  -deps-cache-max-bytes <bytes>
  -deps-sources, -no-deps-sources
  --dev
  -h, --help
      kotlinc language arguments are accepted as well: -language-version, -api-version,
      -progressive, -XXLanguage:, and the -X arguments that switch language features.
";
    let output = Command::new(env!("CARGO_BIN_EXE_krusty-lsp"))
        .arg("--help")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "help must not write a startup error"
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), HELP);
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
