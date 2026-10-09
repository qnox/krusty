//! A kotlinc argument krusty does not implement stops the compilation before anything is written.
//! Accepting one would compile the same command line into something other than what kotlinc
//! produces, so the refusal must hold for diagnostics policy, language semantics and JVM output
//! shape alike. `cli::tests::every_kotlinc_argument_takes_its_dispositions_path` covers every
//! argument of the table; this test runs the shipping binary on representatives of each kind.

use super::common;

const SOURCE: &str = "fun box(): String = \"OK\"\n";

#[test]
fn an_unsupported_kotlinc_argument_exits_nonzero_and_writes_nothing() {
    for argument in [
        // Diagnostics policy.
        "-Werror",
        "-nowarn",
        // Language semantics.
        "-progressive",
        "-Xallow-reified-type-in-catch",
        "-Xjsr305=strict",
        // JVM output shape.
        "-Xemit-jvm-type-annotations",
        "-Xstring-concat=inline",
        "-Xjvm-enable-preview",
        "-Xno-source-debug-extension",
    ] {
        let work = common::scratch_dir().expect("allocate a scratch directory");
        let source = work.join("Box.kt");
        std::fs::write(&source, SOURCE).expect("write the fixture");
        let output = work.join("out");
        let result = std::process::Command::new(common::krusty_binary())
            .arg("-d")
            .arg(&output)
            .arg(argument)
            .arg(&source)
            .output()
            .expect("run krusty");
        assert_eq!(result.status.code(), Some(2), "{argument}");
        assert_eq!(String::from_utf8_lossy(&result.stdout), "", "{argument}");
        assert_eq!(
            String::from_utf8_lossy(&result.stderr),
            format!("krusty: error: krusty does not implement the kotlinc argument '{argument}'\n"),
            "{argument}"
        );
        assert!(!output.exists(), "{argument} wrote {}", output.display());
        let _ = std::fs::remove_dir_all(work);
    }
}
