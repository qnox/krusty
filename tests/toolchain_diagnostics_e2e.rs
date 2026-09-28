//! `krusty-toolchain build` reports a source error as kotlinc does.
//!
//! The ledger comes from the diagnostic harness: kotlinc's `file:line:column: message` for this
//! source, recorded per Kotlin version. The toolchain must print that same diagnostic.

use std::path::PathBuf;
use std::process::Command;

use super::common;

#[test]
fn a_toolchain_build_reports_kotlinc_diagnostics() {
    let source = "fun breakOutside() { break }\n";
    let expected = common::recorded(|| common::reference_error_ledger(&[("main.kt", source)], &[]));
    assert!(!expected.is_empty(), "kotlinc reported no error");

    let root = std::env::temp_dir().join(format!(
        "krusty-toolchain-diag-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::write(root.join("module.yaml"), "product: jvm/app\n").expect("module");
    std::fs::write(root.join("src/main.kt"), source).expect("source");

    let output = Command::new(toolchain_binary())
        .current_dir(&root)
        .env("KRUSTY_COMPILER", common::krusty_binary())
        .arg("build")
        .output()
        .expect("run krusty-toolchain");
    let _ = std::fs::remove_dir_all(&root);

    let stderr = String::from_utf8_lossy(&output.stderr);
    let actual = common::compiler_errors(&stderr)
        .into_iter()
        .map(|error| {
            format!(
                "{}:{}:{}: {}",
                error.file, error.line, error.column, error.message
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        expected,
        "krusty-toolchain stderr:\n{stderr}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_ne!(output.status.code(), Some(0));
}

fn toolchain_binary() -> PathBuf {
    let compiler = common::krusty_binary();
    let binary = compiler
        .parent()
        .expect("krusty directory")
        .join(format!("krusty-toolchain{}", std::env::consts::EXE_SUFFIX));
    if binary.is_file() {
        return binary;
    }
    let profile = compiler
        .parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .expect("profile");
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "-p", "krusty-toolchain"]);
    if profile != "debug" {
        build.args(["--profile", profile]);
    }
    // The coverage gate exports nightly-only `-Z` rustflags, then runs this binary directly, so
    // the flags are inherited. `env!("CARGO")` is the toolchain that compiled the test, which is
    // not that nightly, and rustc rejects the flags while building dependencies. This binary only
    // prints diagnostics; the instrumented compiler is selected with `KRUSTY_COMPILER`.
    build.env_remove("RUSTFLAGS");
    build.env_remove("CARGO_ENCODED_RUSTFLAGS");
    let status = build
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("build krusty-toolchain");
    assert!(status.success(), "failed to build krusty-toolchain");
    assert!(
        binary.is_file(),
        "krusty-toolchain did not produce {}",
        binary.display()
    );
    binary
}
