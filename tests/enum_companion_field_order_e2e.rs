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

/// The member names javap prints, in emission order — fields and methods alike.
fn members(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter(|line| declaration_row(line) && !line.contains('{'))
        .filter_map(|line| {
            let name = line.split('(').next()?.split_whitespace().last()?;
            Some(name.trim_end_matches(';').to_string())
        })
        .collect()
}

/// The field declarations javap prints for `class`, in emission order.
fn fields(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter(|line| declaration_row(line) && !line.contains('('))
        .map(str::to_string)
        .collect()
}

/// Whether a `javap -v` line is a member DECLARATION. Plenty else ends in `;`: constant-pool `Utf8`
/// rows (which start with `#`), `Signature:`/`descriptor:` attribute lines and `LocalVariableTable`
/// rows (which carry a `:`).
fn declaration_row(line: &str) -> bool {
    line.ends_with(';') && !line.starts_with('#') && !line.contains(':')
}

/// Compile the fixture with the reference kotlinc and with krusty; returns both disassemblies.
fn build_both() -> Option<(String, String)> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let source = dir.join("EnumCompanionFieldOrder.kt");
    std::fs::write(&source, SRC).expect("write fixture");

    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ])?;
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

    let reference = common::javap(&["-p", "-v", "-cp", &reference_dir.to_string_lossy(), "Plain"])?;
    let krusty = common::javap(&["-p", "-v", "-cp", &krusty_dir.to_string_lossy(), "Plain"])
        .expect("javap reads krusty's output");
    let _ = std::fs::remove_dir_all(&dir);
    Some((reference, krusty))
}

#[test]
fn an_enum_emits_its_companion_field_first() {
    let Some((reference, krusty)) = build_both() else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = fields(&reference);
    assert!(
        want.first()
            .is_some_and(|first| first.contains("Companion")),
        "reference must put the companion field first — that is the rule under test:\n{reference}"
    );
    assert_eq!(fields(&krusty), want, "field order");
}

/// kotlinc's enum member order: `<init>`, the DECLARED members, then the synthesized
/// `values`/`valueOf`/`getEntries`/`$values`, then `<clinit>`. krusty emitted the declared members
/// AFTER the synthesized ones, so a property accessor landed past `$values`.
#[test]
fn an_enum_emits_declared_members_before_its_synthesized_ones() {
    let Some((reference, krusty)) = build_both() else {
        return;
    };
    let want = members(&reference);
    let values = want.iter().position(|name| name == "values");
    let accessor = want.iter().position(|name| name == "getTag");
    assert!(
        matches!((accessor, values), (Some(a), Some(v)) if a < v),
        "reference must declare getTag before values():\n{reference}"
    );
    assert_eq!(members(&krusty), want, "member order");
}

/// kotlinc's constant-pool visit order for an enum: the class and supertype, the constructor's names
/// and descriptors and its `super(name, ordinal)` reference, the property backing fields WITH their
/// `NameAndType`/`Fieldref`, the constructor's LocalVariableTable strings, the DECLARED accessors,
/// then `values`/`valueOf`/`getEntries`/`$VALUES`/`$ENTRIES`, then `<clinit>` AND its descriptor,
/// then the entry constants. The `Companion` field leads the field TABLE but interns late, with the
/// field visit.
///
/// krusty front-loaded the fields and the synthesized machinery, which shifted almost the whole pool
/// even where every member already matched.
#[test]
fn an_enum_interns_its_pool_in_kotlincs_order() {
    let Some((reference, krusty)) = build_both() else {
        return;
    };
    let pool = |text: &str| {
        text.lines()
            .map(str::trim)
            .filter(|line| line.starts_with('#') && line.contains(" = "))
            .map(|line| {
                // Drop the index; keep the entry's kind and payload.
                line.split_once(" = ")
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
    };
    let want = pool(&reference);
    assert!(
        want.len() > 40,
        "reference pool should be substantial:\n{reference}"
    );
    let got = pool(&krusty);
    if got != want {
        let first = want
            .iter()
            .zip(got.iter())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| want.len().min(got.len()));
        panic!(
            "constant pool diverges at entry {}\n  kotlinc: {:?}\n  krusty:  {:?}\n  ({} of {} entries differ)",
            first + 1,
            want.get(first),
            got.get(first),
            want.iter().zip(got.iter()).filter(|(a, b)| a != b).count(),
            want.len(),
        );
    }
}

/// An enum constructor maps each property store to the PARAMETER's own line and the trailing
/// `return` back to the class header — the same three-entry shape kotlinc gives an ordinary class,
/// which the enum path did not share: it put every store on the header line and wrote no closing
/// entry.
///
/// Only a parameter list spanning LINES can show it. Every enum fixture in the suite declared its
/// parameters on the header line, where all three lines coincide and the entries dedupe to one.
#[test]
fn an_enum_constructor_maps_each_property_store_to_its_parameter_line() {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skipping: no scratch dir");
        return;
    };
    let source = dir.join("EnumCtorLines.kt");
    let src = "enum class Shape(\n\
               \x20   val sides: Int,\n\
               \x20   val label: String,\n\
               ) {\n\
               \x20   TRIANGLE(3, \"tri\"),\n\
               \x20   SQUARE(4, \"sq\"),\n\
               }\n";
    std::fs::write(&source, src).expect("write fixture");
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
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
    let _reference = std::fs::read(reference_dir.join("Shape.class")).expect("reference class");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let classes = common::compile_in_process_metadata_cp_module_target(
        src,
        "EnumCtorLines",
        &[],
        "main",
        Some(69),
    )
    .expect("krusty compiles the fixture");
    for (internal, bytes) in &classes {
        std::fs::write(krusty_dir.join(format!("{internal}.class")), bytes)
            .expect("write krusty class");
    }
    // The `line N: pc` rows of the constructor, which javap prints under its Code attribute.
    let constructor_lines = |disassembly: &str| {
        let start = disassembly
            .find("Shape(java.lang.String, int")
            .or_else(|| disassembly.find("Shape("))
            .unwrap_or(0);
        disassembly[start..]
            .lines()
            .skip_while(|line| !line.contains("LineNumberTable"))
            .skip(1)
            .take_while(|line| line.trim().starts_with("line "))
            .map(|line| line.trim().to_string())
            .collect::<Vec<_>>()
    };
    let Some(reference) =
        common::javap(&["-p", "-v", "-cp", &reference_dir.to_string_lossy(), "Shape"])
    else {
        eprintln!("skipping: javap unavailable");
        return;
    };
    let krusty = common::javap(&["-p", "-v", "-cp", &krusty_dir.to_string_lossy(), "Shape"])
        .expect("javap reads krusty's output");
    let want = constructor_lines(&reference);
    assert!(
        want.len() >= 3,
        "reference must map more than the `super` call — the rule under test: {want:?}\n{reference}"
    );
    assert_eq!(constructor_lines(&krusty), want, "<init> LineNumberTable");
    let _ = std::fs::remove_dir_all(&dir);
}
