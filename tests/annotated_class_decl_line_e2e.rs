//! A class whose annotation sits on its own line got the wrong primary-constructor
//! `LineNumberTable`, so no annotated class could be byte-identical to kotlinc.
//!
//! kotlinc uses TWO lines for a primary constructor, and they are not the same line:
//!
//!   * the `super()` call maps to where the DECLARATION starts — annotations included;
//!   * the trailing `return` maps back to the class HEADER line.
//!
//! They coincide for an unannotated class, which is why one line (`ClassDecl::decl_line`, the
//! header) served for both and every fixture stayed green. Put `@Mark` on the line above and
//! kotlinc emits `line 5 → super()`, `line 6 → the property stores`, while krusty emitted a single
//! `line 6` entry. Naively moving `decl_line` to the annotation is wrong in the other direction: the
//! trailing `return` then lands on line 5 too and kotlinc has no such entry.
//!
//! `@Serializable`, `@Entity`, `@JsonClass` and friends put this on the hot path for real code —
//! every annotated class in a project carries it.
use super::common;

#[test]
fn annotated_class_ctor_line_table_matches_kotlinc() {
    // The annotation MUST be on its own line — that is the whole difference under test.
    let src = "annotation class Mark\n\
               \n\
               @Mark\n\
               data class Pair2(val x: Int, val y: String)\n";
    let Some(result) = common::byte_diff_against_kotlinc("AnnotatedDeclLine", src, "Pair2") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Pair2 byte-identical to kotlinc");
}

/// The header and the declaration start coincide here, so this pins that the fix changes nothing
/// for an unannotated class — including the trailing `return` entry, which a multi-line header
/// makes visible as a separate line.
#[test]
fn unannotated_multiline_header_ctor_line_table_is_unchanged() {
    let src = "data class Multi(\n\
               \x20   val a: Int,\n\
               \x20   val b: String,\n\
               )\n";
    let Some(result) = common::byte_diff_against_kotlinc("MultilineHeader", src, "Multi") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Multi byte-identical to kotlinc");
}

/// The class annotation moved the `super()` call's line; a PROPERTY annotation moves a different
/// entry — the constructor's store of that property.
///
/// kotlinc maps a primary-constructor property's field store to the line its declaration STARTS on,
/// annotations included, while the property's own getter stays on the `val` line. krusty had one
/// line for both, so every constructor property with an annotation above it put its store one line
/// too low. `@SerialName` over a generated model's property is exactly this shape.
#[test]
fn an_annotated_constructor_property_stores_on_its_declaration_start_line() {
    let src = "annotation class Mark\n\
               \n\
               data class Tagged(\n\
               \x20   @Mark\n\
               \x20   val first: Int,\n\
               \x20   val second: Int,\n\
               )\n";
    // The line table alone, not the whole class: a plain annotation on a constructor property also
    // brings a `get<X>$annotations` marker krusty does not yet emit, and that independent gap would
    // fail a whole-class comparison.
    assert_ctor_line_table_matches(
        "AnnotatedCtorProperty",
        src,
        "Tagged",
        &["3: 0", "4: 4", "6: 9", "3: 14"],
    );
}

/// The same annotation on a BODY property moves nothing: kotlinc leaves that initializer's store on
/// the property's own line. Measured, not assumed — it is the reason the constructor's start line is
/// a separate fact instead of a correction applied to every property.
#[test]
fn an_annotated_body_property_keeps_its_own_line() {
    let src = "annotation class Mark\n\
               \n\
               class Held {\n\
               \x20   @Mark\n\
               \x20   val value: Int = 7\n\
               }\n";
    assert_ctor_line_table_matches(
        "AnnotatedBodyProperty",
        src,
        "Held",
        &["3: 0", "5: 4", "3: 10"],
    );
}

/// A primary-constructor parameter that is no property (here the delegate of `by`) has no field,
/// so the constructor's tables must be keyed by its parameters. Keyed by its first fields, the
/// primary's line and local tables went to the all-defaults `<init>()` instead: the primary shipped
/// with none, and `<init>()` got a line entry in the middle of its `invokespecial`.
#[test]
fn a_delegating_parameter_keeps_the_primary_constructor_table() {
    let src = "interface Source { fun get(): String }\n\
               class Impl : Source { override fun get() = \"x\" }\n\
               class Wrapped(source: Source = Impl()) : Source by source\n";
    assert_ctor_line_table_matches("DelegatingParameter", src, "Wrapped", &["3: 6"]);
}

/// Compare one class's primary-constructor `LineNumberTable` against kotlinc's, exactly.
///
/// Fails CLOSED: an empty table on either side is a broken differential, not agreement.
fn assert_ctor_line_table_matches(name: &str, src: &str, class: &str, expected: &[&str]) {
    let dir = common::scratch_dir()
        .unwrap_or_else(|| panic!("{name}: no scratch directory for the line differential"));
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("create reference directory");
    std::fs::create_dir_all(&krusty_dir).expect("create output directory");
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).expect("write differential source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .unwrap_or_else(|| panic!("{name}: reference kotlinc unavailable under the test harness"));
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process(src, name, &[common::stdlib_jar()], None)
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile the fixture"));
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create class directory");
        }
        std::fs::write(path, bytes).expect("write emitted class");
    }
    let table = |dir: &std::path::Path, side: &str| -> Vec<String> {
        let classpath = dir.to_string_lossy().into_owned();
        let text = common::javap(&["-p", "-c", "-v", "-cp", classpath.as_str(), class])
            .unwrap_or_else(|| panic!("{name}: javap unavailable for the {side} class"));
        let rows = ctor_line_rows(&text, class);
        assert!(
            !rows.is_empty(),
            "{name}: the {side} {class} has no constructor LineNumberTable"
        );
        rows
    };
    let want = table(&reference_dir, "kotlinc");
    assert_eq!(want, expected, "{name}: kotlinc's exact constructor table");
    assert_eq!(
        table(&krusty_dir, "krusty"),
        want,
        "{name}: {class}'s constructor LineNumberTable"
    );
}

/// The `line N: pc` rows of the FIRST `<init>` in a `javap -c -v` dump.
fn ctor_line_rows(text: &str, class: &str) -> Vec<String> {
    let signature = format!("{class}(");
    let mut inside_ctor = false;
    let mut inside_table = false;
    let mut rows = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.ends_with(");") && line.contains(&signature) {
            inside_ctor = true;
        }
        if !inside_ctor {
            continue;
        }
        if line == "LineNumberTable:" {
            inside_table = true;
            continue;
        }
        if inside_table {
            if let Some(row) = line.strip_prefix("line ") {
                rows.push(row.to_string());
                continue;
            }
            return rows;
        }
    }
    rows
}
