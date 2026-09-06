//! A `-cp` entry the classpath reader cannot open used to vanish without a word: the compile then
//! failed with `unresolved reference` on every import from that jar and nothing in the output
//! pointed at the jar. kotlinc warns and continues (`classpath entry points to a non-existent
//! location`, `WARN: Error while reading zip file`); krusty now says so on stderr the same way.

use super::common;
use std::process::Command;

fn compile_with_cp(name: &str, cp_entry: &std::path::Path) -> (bool, String, bool) {
    let dir = std::env::temp_dir().join(format!("krusty_cpwarn_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("Main.kt");
    std::fs::write(&src, "fun main() { println(\"x\") }\n").unwrap();
    let cp = format!("{}:{}", cp_entry.display(), common::stdlib_jar().display());
    let out = Command::new(common::krusty_binary())
        .args(["-no-stdlib", "-no-jdk", "-cp", &cp, "-d"])
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("run krusty");
    let emitted = dir.join("MainKt.class").exists();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        emitted,
    )
}

#[test]
fn a_missing_classpath_entry_warns_in_kotlinc_words_and_compiles() {
    let missing = std::path::PathBuf::from("/definitely/not/there.jar");
    let (ok, stderr, emitted) = compile_with_cp("missing", &missing);
    assert!(
        stderr.contains(
            "warning: classpath entry points to a non-existent location: /definitely/not/there.jar"
        ),
        "stderr:\n{stderr}"
    );
    assert!(
        ok && emitted,
        "the compile must still succeed; stderr:\n{stderr}"
    );
}

#[test]
fn a_truncated_jar_on_the_classpath_warns_and_compiles() {
    let dir = std::env::temp_dir().join(format!("krusty_cpwarn_jar_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let jar = dir.join("core.jar");
    std::fs::write(&jar, b"PK\x03\x04 not really a zip").unwrap();
    let (ok, stderr, emitted) = compile_with_cp("truncated", &jar);
    assert!(
        stderr.contains(&format!(
            "warning: cannot read classpath entry {}: ",
            jar.display()
        )),
        "stderr:\n{stderr}"
    );
    assert!(
        ok && emitted,
        "the compile must still succeed; stderr:\n{stderr}"
    );
}
