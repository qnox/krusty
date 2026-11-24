//! A Java method's parameter names are declared, not callable.
//!
//! A `MethodParameters` entry names a Java parameter, and krusty now keeps those names so a
//! diagnostic can render a Java declaration as its author wrote it. Kotlin still passes no argument
//! to a Java method by name, so the names must never reach argument mapping: the reference
//! compiler rejects `target.add(amount = 1L)` for a Java `add(long amount)`, and so must krusty.

use super::common;

#[test]
fn a_java_methods_declared_parameter_name_is_not_a_named_argument() {
    let dir = common::scratch_dir().expect("scratch dir");
    let java = dir.join("Target.java");
    std::fs::write(
        &java,
        "public class Target {\n    public long add(long amount) {\n        return amount;\n    }\n}\n",
    )
    .expect("write Java source");
    let kotlin = dir.join("Use.kt");
    std::fs::write(
        &kotlin,
        "fun use(target: Target): Long = target.add(amount = 1L)\n",
    )
    .expect("write Kotlin source");
    let stdlib = common::stdlib_jar().to_string_lossy().into_owned();

    let reference_out = dir.join("reference");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (status, reference) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        stdlib.clone(),
        java.to_string_lossy().into_owned(),
        kotlin.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    assert_ne!(status, 0, "the reference compiler rejects it:\n{reference}");

    let out = std::process::Command::new(common::krusty_binary())
        .args(["-no-stdlib", "-cp", &stdlib, "-d"])
        .arg(dir.join("krusty"))
        .args([&java, &kotlin])
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "krusty rejects it:\n{krusty}");
    // Both reject the one argument at the same place. The reference compiler words it "named
    // arguments are prohibited for non-Kotlin functions" where krusty reports that no parameter
    // of that name exists; that wording is a separate gap, so only the position is pinned here.
    let place = |report: &str| {
        common::reported(report)
            .into_iter()
            .map(|error| (error.file, error.line, error.column))
            .collect::<Vec<_>>()
    };
    assert_eq!(place(&krusty), place(&reference));
    assert_eq!(place(&reference).len(), 1);
}
