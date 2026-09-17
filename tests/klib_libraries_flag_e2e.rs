//! `-libraries`: a KLIB dependency reaching resolution, through the driver.
//!
//! kotlinc spells a KLIB dependency `-libraries`, and this is the flag's whole observable point —
//! a reference that is unresolved without it resolves with it. The unit tests cover the parse; this
//! runs the compiler, because a flag that parses into a field nothing reads would pass those and
//! still do nothing.

use super::common;
use std::process::Command;

/// Compile a source referencing a Kotlin/Native-only classifier, with and without the klib.
fn compile(source: &str, libraries: Option<&std::path::Path>) -> (bool, String) {
    let dir = common::scratch_dir().expect("scratch dir");
    let src = dir.join("Main.kt");
    std::fs::write(&src, source).unwrap();
    let mut command = Command::new(common::krusty_binary());
    command.args(["-no-stdlib", "-no-jdk", "-cp"]);
    command.arg(common::stdlib_jar());
    if let Some(libraries) = libraries {
        command.arg("-libraries").arg(libraries);
    }
    let out = command
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), report)
}

/// `kotlin.native.ref.WeakReference` is in the Native stdlib klib and in no jar on the classpath,
/// so it is exactly the reference the flag has to supply.
const SOURCE: &str = "fun keep(reference: kotlin.native.ref.WeakReference<Any>) = reference\n";

#[test]
fn a_klib_only_reference_is_unresolved_without_the_flag() {
    let (_, report) = compile(SOURCE, None);
    assert!(
        report.contains("unresolved reference"),
        "without -libraries the classifier is nowhere on the dependency path:\n{report}"
    );
}

#[test]
fn a_klib_only_reference_resolves_with_the_flag() {
    let Some(stdlib) = krusty::toolchain::kotlin_native_stdlib() else {
        return;
    };
    let (_, report) = compile(SOURCE, Some(&stdlib));
    assert!(
        !report.contains("unresolved reference"),
        "-libraries puts the klib's declarations on the dependency path:\n{report}"
    );
}

/// A path that is not a klib contributes nothing and is not an error — the same thing an absent
/// library does. kotlinc does not fail a compile over an unreadable `-libraries` entry either.
#[test]
fn a_libraries_entry_that_is_not_a_klib_is_not_fatal() {
    let dir = common::scratch_dir().expect("scratch dir");
    let not_a_klib = dir.join("nonesuch.klib");
    std::fs::write(&not_a_klib, b"not a klib").unwrap();
    let (ok, report) = compile("fun main() {}\n", Some(&not_a_klib));
    assert!(ok, "the compile still runs:\n{report}");
}
