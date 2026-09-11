//! An ENUM's `Companion` field comes FIRST in kotlinc's field order — ahead of the constructor
//! properties, the entry constants and `$VALUES`/`$ENTRIES` — unlike an ordinary class, where the
//! companion field follows the instance fields. krusty emitted it last on the enum path.
//!
//! The ordering is not cosmetic: `add_field` interns the field's name and descriptor, so emitting
//! the companion last also interned those strings late and left the class differing in constant-pool
//! order even where every member matched.
//!
//! This asserts the field ORDER rather than byte identity. An enum carrying a companion is not yet
//! byte-identical for unrelated reasons (its method order and `LineNumberTable` still differ), and a
//! byte assertion here would fail for those instead — hiding a regression in the thing under test.
use super::common;

const SRC: &str = "enum class Plain(val tag: String) {\n\
                   \x20   A(\"a\"), B(\"b\");\n\
                   \x20\n\
                   \x20   companion object { fun first(): Plain = A }\n\
                   }\n";

/// The field declarations javap prints for `class`, in emission order.
fn fields(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter(|line| line.ends_with(';') && !line.contains('('))
        .map(str::to_string)
        .collect()
}

#[test]
fn an_enum_emits_its_companion_field_first() {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skipping: no scratch directory");
        return;
    };
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let source = dir.join("EnumCompanionFieldOrder.kt");
    std::fs::write(&source, SRC).expect("write fixture");

    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp_module_target(
        SRC,
        "EnumCompanionFieldOrder",
        &[],
        "main",
        Some(69),
    )
    .expect("krusty compiles the fixture");
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("krusty class directory");
        }
        std::fs::write(path, bytes).expect("write krusty class");
    }

    let Some(reference) = common::javap(&["-p", "-cp", &reference_dir.to_string_lossy(), "Plain"])
    else {
        eprintln!("skipping: javap unavailable");
        return;
    };
    let krusty = common::javap(&["-p", "-cp", &krusty_dir.to_string_lossy(), "Plain"])
        .expect("javap reads krusty's output");
    let _ = std::fs::remove_dir_all(&dir);

    let want = fields(&reference);
    assert!(
        want.first()
            .is_some_and(|first| first.contains("Companion")),
        "reference must put the companion field first — that is the rule under test:\n{reference}"
    );
    assert_eq!(fields(&krusty), want, "field order");
}
