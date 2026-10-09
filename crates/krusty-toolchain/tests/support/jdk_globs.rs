//! The JDK's `glob:` path matcher, after the toolchain's normalization of a module glob, through
//! `scripts/kotlin-toolchain/GlobOracle.java` on the JDK `JAVA_HOME` names.

use std::path::PathBuf;
use std::process::Command;

use super::oracle::{run_merged, scratch, script, Fingerprint, Output, Reference};

/// What `GlobOracle.java` prints for `cases` (one `pattern<TAB>path<TAB>path…` per line) on the JDK
/// `JAVA_HOME` names: replayed from the cache, or run when allowed.
pub fn jdk_globs(case: &str, cases: &str) -> Output {
    let java_home = PathBuf::from(std::env::var_os("JAVA_HOME").expect("JAVA_HOME names a JDK"));
    let release = std::fs::read(java_home.join("release")).expect("read the JDK's `release`");
    let version = String::from_utf8_lossy(&release)
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))
        .expect("the JDK's `release` names its version")
        .trim_matches('"')
        .to_string();
    let oracle = std::fs::read(script("GlobOracle.java")).expect("read GlobOracle.java");
    let reference = Reference {
        slot: format!("jdk-{version}"),
        fingerprint: Fingerprint::of(&[b"jdk-globs", &release, &oracle, cases.as_bytes()]),
        case,
    };
    reference.output(|| {
        let directory = scratch();
        std::fs::write(directory.join("cases.tsv"), cases).unwrap();
        let compiled = Command::new(java_home.join("bin/javac"))
            .arg("-d")
            .arg(&directory)
            .arg(script("GlobOracle.java"))
            .status()
            .expect("run javac");
        assert!(compiled.success(), "javac GlobOracle.java failed");
        let mut command = Command::new(java_home.join("bin/java"));
        command
            .arg("-cp")
            .arg(&directory)
            .arg("GlobOracle")
            .arg("cases.tsv")
            .current_dir(&directory);
        let output = run_merged(command, &directory);
        let _ = std::fs::remove_dir_all(&directory);
        output
    })
}
