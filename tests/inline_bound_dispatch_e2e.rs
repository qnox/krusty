//! A member call inside an inline function keeps the dispatch class selected at the declaration.
//!
//! Substituting `T : Number` with `Long` must not retarget `toDouble` to `Long.doubleValue`
//! inside a lambda whose parameter is still erased to `Number`. Substituting
//! `T : IntProgression` with `IntRange` keeps `IntProgression.getFirst`.

use std::path::{Path, PathBuf};

use super::common;

const MEDIAN: &str = r#"
inline fun <T : Number> List<T>.med(): Double {
    val sorted = this.sortedBy { it.toDouble() }
    return sorted[0].toDouble()
}

fun box(): String {
    val xs = listOf(1L)
    return if (xs.med() == 1.0) "OK" else "fail"
}
"#;

const PROGRESSION: &str = r#"
inline fun <T : IntProgression> T.firstOf(): Int = this.first

fun direct(): Int = (1..3).first

fun box(): String {
    val r = 1..3
    return if (r.firstOf() == 1 && direct() == 1) "OK" else "fail"
}
"#;

struct Compiled {
    krusty: PathBuf,
    reference: PathBuf,
}

fn compile_pair(name: &str, source: &str) -> Compiled {
    let root = common::scratch_dir().expect("scratch dir");
    let krusty = root.join("krusty");
    let reference = root.join("ref");
    std::fs::create_dir_all(&krusty).unwrap();
    std::fs::create_dir_all(&reference).unwrap();
    let classes = common::compile_in_process_metadata_cp(source, name, &[common::stdlib_jar()])
        .unwrap_or_else(|| panic!("{name}: krusty rejected the source"));
    for (internal, bytes) in classes {
        let path = krusty.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }
    let source_path = root.join(format!("{name}.kt"));
    std::fs::write(&source_path, source).unwrap();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        "-classpath".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])
    .expect("kotlinc");
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    Compiled { krusty, reference }
}

fn class_file(dir: &Path, needle: &str) -> PathBuf {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.contains(needle) && name.ends_with(".class") {
            found.push(path);
        }
    }
    assert_eq!(found.len(), 1, "{needle} in {}", dir.display());
    found.pop().unwrap()
}

fn disassemble(path: &Path) -> String {
    common::javap(&["-c", "-p", &path.to_string_lossy()]).expect("javap")
}

fn method_invokes(disassembly: &str, marker: &str) -> Vec<String> {
    common::method_instructions(disassembly, marker)
        .into_iter()
        .filter_map(|line| {
            let (_, instruction) = line.split_once(": ")?;
            instruction
                .contains("invoke")
                .then(|| instruction.to_string())
        })
        .collect()
}

#[test]
fn an_inlined_number_bound_keeps_number_double_value() {
    let output =
        common::compile_and_run_with_stdlib(MEDIAN, "Median").expect("median compiles and runs");
    assert_eq!(output, "OK");

    let compiled = compile_pair("Median", MEDIAN);
    let krusty = disassemble(&class_file(&compiled.krusty, "sortedBy"));
    let reference = disassemble(&class_file(&compiled.reference, "sortedBy"));
    let krusty_invokes = method_invokes(&krusty, "int compare(");
    let reference_invokes = method_invokes(&reference, "int compare(");
    assert!(
        !krusty_invokes.is_empty() && !reference_invokes.is_empty(),
        "compare() invokes\nkrusty: {krusty_invokes:?}\nkotlinc: {reference_invokes:?}"
    );
    assert_eq!(krusty_invokes, reference_invokes);
    assert!(
        krusty_invokes
            .iter()
            .any(|line| line.contains("java/lang/Number.doubleValue:()D")),
        "{krusty_invokes:?}"
    );
}

#[test]
fn an_inlined_progression_keeps_the_bound_owner() {
    let output = common::compile_and_run_with_stdlib(PROGRESSION, "Progression")
        .expect("progression compiles and runs");
    assert_eq!(output, "OK");

    let compiled = compile_pair("Progression", PROGRESSION);
    let krusty = disassemble(&compiled.krusty.join("ProgressionKt.class"));
    let reference = disassemble(&compiled.reference.join("ProgressionKt.class"));
    assert_eq!(
        method_invokes(&krusty, "String box("),
        method_invokes(&reference, "String box(")
    );
    assert_eq!(
        method_invokes(&krusty, "int direct("),
        method_invokes(&reference, "int direct(")
    );
    let box_invokes = method_invokes(&krusty, "String box(");
    assert!(
        box_invokes
            .iter()
            .any(|line| line.contains("kotlin/ranges/IntProgression.getFirst:()I")),
        "{box_invokes:?}"
    );
    let direct_invokes = method_invokes(&krusty, "int direct(");
    assert!(
        direct_invokes
            .iter()
            .any(|line| line.contains("kotlin/ranges/IntRange.getFirst:()I")),
        "{direct_invokes:?}"
    );
}
