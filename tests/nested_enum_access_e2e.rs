//! A nested `enum class` declared in a class body is hoisted to a top-level `Outer.Inner` type
//! (internal `Outer$Inner`) and its entries are read through the enclosing type name
//! (`Outer.Inner.ENTRY`). The checker types the qualified access as the enum's own type and the
//! lowerer emits the enum-constant read.

use crate::common;

fn check(rel: &str) {
    match common::run_box_corpus_case(rel) {
        Some(s) => assert_eq!(s, "OK", "{rel}"),
        None => panic!("unexpectedly skipped: {rel}"),
    }
}

#[test]
fn nested_enum_entry_via_outer_name() {
    check("enum/inner.kt");
}

#[test]
fn nested_enum_with_body_and_value() {
    // A nested enum with a constructor and a member, read through the outer name.
    let src = r#"
class Palette {
    enum class Color(val hex: Int) {
        RED(0xff0000),
        GREEN(0x00ff00)
    }
}

fun box(): String {
    val c = Palette.Color.GREEN
    return if (c.hex == 0x00ff00 && c.name == "GREEN") "OK" else "FAIL"
}
"#;
    common::expect_box_ok_with_stdlib(src, "NestedEnumBody");
}

#[test]
fn nested_enum_entry_uses_lexical_class_scope() {
    let src = r#"
class Catalog {
    fun read(): String {
        val state = Mode.READY
        return if (state.name == "READY") "OK" else "FAIL"
    }

    private enum class Mode {
        READY
    }
}

fun box(): String = Catalog().read()
"#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(src) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
    common::expect_box_ok_with_stdlib(src, "LexicalNestedEnum");
}

#[test]
fn nested_classifier_shadows_top_level_enum() {
    let src = r#"
enum class Mode {
    READY
}

class Catalog {
    class Mode

    fun read(): Any = Mode.READY
}
"#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(src) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic == "unresolved reference 'Mode'."),
        "expected the shadowed enum entry to stay unresolved, got: {diagnostics:?}"
    );
}

#[test]
fn enum_typed_static_property_is_not_an_enum_entry() {
    let src = r#"
enum class Mode {
    READY,
    OTHER
}

class Holder {
    companion object {
        val READY: Mode = Mode.OTHER
    }
}

fun box(): String = if (Holder.READY == Mode.OTHER) "OK" else "FAIL"
"#;

    common::expect_box_ok_with_stdlib(src, "EnumTypedStaticProperty");
}

#[test]
fn enum_entry_from_another_source_file_uses_its_declaring_owner() {
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    let output = common::compile_and_run_files_with_stdlib(&[
        (
            "State",
            r#"
package sample

enum class State {
    READY
}
"#,
        ),
        (
            "Main",
            r#"
package app

import sample.State

fun box(): String = if (State.READY.name == "READY") "OK" else "FAIL"
"#,
        ),
    ]);
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn nullable_enum_subject_accepts_non_null_entries() {
    let src = r#"
enum class Phase {
    READY,
    WAITING
}

fun renderPhase(phase: Phase?): String {
    when (phase) {
        Phase.READY -> return "ready"
        Phase.WAITING -> return "waiting"
        null -> Unit
    }
    return "none"
}

fun box(): String =
    if (renderPhase(Phase.READY) == "ready" && renderPhase(null) == "none") "OK" else "FAIL"
"#;

    common::expect_box_ok_with_stdlib(src, "NullableEnumWhen");
}

#[test]
fn nullable_when_subject_comparability_is_type_generic() {
    let source = r#"
object Marker

fun renderMarker(marker: Marker?): String =
    when (marker) {
        Marker -> "marker"
        null -> "none"
        else -> "other"
    }

fun renderNumber(number: Int?): String =
    when (number) {
        1 -> "one"
        null -> "none"
        else -> "other"
    }
"#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(source) else {
        return;
    };
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
