//! A nullable function value adapted to a nullable fun interface stays null.
//!
//! Wrapping the null produces a non-null SAM whose method throws. A non-null function and a
//! lambda literal still become the interface.

use std::path::Path;

use super::common;

const SRC: &str = r#"
fun interface KRunnable {
    fun invoke()
}

fun isNull(r: KRunnable?): Boolean {
    if (r == null) return true
    r.invoke()
    return false
}

fun nullableFun(fromNull: Boolean): (() -> Unit)? =
    if (fromNull) null else {{}}

fun box(): String {
    if (!isNull(nullableFun(true))) return "Fail 1"
    if (isNull(nullableFun(false))) return "Fail 2"
    if (!isNull(null)) return "Fail 3"
    if (isNull {}) return "Fail 4"
    return "OK"
}
"#;

#[test]
fn a_nullable_function_value_stays_null_as_a_fun_interface() {
    common::expect_box_ok_with_stdlib(SRC, "NullableSam");
}

#[test]
fn a_nullable_function_value_stays_null_under_class_sam_conversion() {
    let work = common::scratch_dir().expect("allocate nullable SAM class-strategy fixture");
    let source = work.join("NullableSam.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create output");
    std::fs::write(&source, SRC).expect("write fixture");
    let stdlib = common::stdlib_jar();
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output"), "-no-reflect"])
        .args(["-Xlambdas=class", "-Xsam-conversions=class"])
        .args(["-classpath", stdlib.to_str().expect("UTF-8 stdlib")])
        .arg(&source)
        .output()
        .expect("run krusty");
    assert!(
        result.status.success(),
        "krusty rejected class SAM conversion: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let classes = class_bytes(&output);
    let outcome = common::run_box(&classes, "NullableSamKt", &[stdlib]);
    let _ = std::fs::remove_dir_all(work);
    assert_eq!(outcome.as_deref(), Some("OK"));
}

fn class_bytes(output: &Path) -> Vec<(String, Vec<u8>)> {
    let mut names = Vec::new();
    collect_classes(output, output, &mut names);
    names
        .into_iter()
        .map(|name| {
            let bytes = std::fs::read(output.join(format!("{name}.class"))).expect("read class");
            (name, bytes)
        })
        .collect()
}

fn collect_classes(root: &Path, dir: &Path, classes: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_classes(root, &path, classes);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            classes.push(
                path.strip_prefix(root)
                    .expect("class below output root")
                    .with_extension("")
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
    }
}
