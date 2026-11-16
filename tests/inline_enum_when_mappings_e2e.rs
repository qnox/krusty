//! kotlinc's `MappedEnumWhenLowering` numbers a class's `$WhenMappings` tables, and an enum's keys
//! in them, as it visits the class's declarations in source order, before any code is generated or
//! inlined. A same-module caller that inlines a compiled inline function keeps reading the callee
//! class's tables, so the callee's numbering must not depend on when its body is compiled.
use super::common;

/// Tables claimed in source order: a top-level function, then a lambda in a property initializer,
/// then another function; in a class, a method, then a property initializer, then an `init`
/// block, with a lambda counted in its class and an object expression owning its own tables.
const SOURCE_ORDER: &str = r#"
enum class A { X, Y }
enum class B { P, Q }
enum class E { M, N }

fun f(b: B) = when (b) { B.Q -> 1; B.P -> 2 }
val g = { a: A -> when (a) { A.Y -> 1; A.X -> 2 } }
fun h(e: E) = when (e) { E.N -> 1; E.M -> 2 }

class C(val a: A, val b: B, val e: E) {
    fun m() = when (e) { E.N -> 1; E.M -> 2 }
    val p = when (b) { B.Q -> 1; B.P -> 2 }
    init { println(when (a) { A.Y -> 1; A.X -> 2 }) }
    val o = object { fun k() = when (a) { A.X -> 1; A.Y -> 2 } }
    fun l() = listOf(a).map { when (it) { A.Y -> 1; A.X -> 2 } }
}
"#;

/// A caller declared before an inline function with two tables, whose class's own later `when`
/// claims a third enum first: the callee's tables are numbered after the caller's own.
const TWO_TABLES: &str = include_str!("box_regressions/inlineTwoWhenMappingTablesBeforeOwnUses.kt");

/// An inline member of a class of the same file, inlined into a top-level function: the inlined
/// code reads the member's class's tables.
const MEMBER: &str = include_str!("box_regressions/inlineMemberWhenMapping.kt");

/// A lambda written in a public inline function is a class of its own whose tables are part of
/// that function's ABI.
const PUBLIC_INLINE_LAMBDA: &str = r#"
enum class Size { S, M, L }

inline fun label(s: Size): () -> String = { when (s) { Size.M -> "m"; else -> "x" } }
"#;

/// An enum `when` that only a top-level function's `$default` wrapper evaluates. `box` passes
/// both arguments, so no call site copies the default.
const DEFAULT_WRAPPER: &str = include_str!("box_regressions/topLevelDefaultWhenMapping.kt");

/// An enum `when` that only a public inline member's `$default` wrapper evaluates: the default is
/// code of that inline function's scope, so its class's mappings carry the public-ABI flag.
const MEMBER_DEFAULT: &str = include_str!("box_regressions/inlineMemberDefaultWhenMapping.kt");

/// An inline function whose file numbers its own tables, in `Decl.kt`, called from `Caller.kt`.
const CROSS_FILE: &str = include_str!("box_regressions/crossFileInlineWhenMapping.kt");

/// Kotlin's own program returns `OK` first, then krusty's does.
fn assert_runs_like_kotlinc(sources: &[(&str, &str)], main_class: &str) {
    assert_eq!(
        common::kotlinc_box_files_result(sources, main_class),
        "OK",
        "kotlinc's {main_class}"
    );
    common::expect_box_ok_files_with_stdlib(sources, main_class);
}

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn a_classs_tables_are_numbered_in_source_order_like_kotlinc() {
    assert_like_kotlinc(&[("Order.kt", SOURCE_ORDER)]);
}

#[test]
fn an_inline_function_with_two_tables_called_before_its_classs_own_uses_matches_kotlinc() {
    assert_like_kotlinc(&[("Two.kt", TWO_TABLES)]);
}

#[test]
fn an_inline_function_with_two_tables_called_before_its_classs_own_uses_runs() {
    assert_runs_like_kotlinc(&[("Two.kt", TWO_TABLES)], "TwoKt");
}

#[test]
fn a_same_file_inline_member_reads_its_classs_tables_like_kotlinc() {
    assert_like_kotlinc(&[("Member.kt", MEMBER)]);
}

#[test]
fn a_same_file_inline_member_reads_its_classs_tables_and_runs() {
    assert_runs_like_kotlinc(&[("Member.kt", MEMBER)], "MemberKt");
}

#[test]
fn a_public_inline_functions_lambda_marks_its_tables_public_abi_like_kotlinc() {
    assert_like_kotlinc(&[("Label.kt", PUBLIC_INLINE_LAMBDA)]);
}

/// The declaration's file numbers its own tables as kotlinc does. A caller in another file still
/// expands the callee in IR (`EnumSwitchInBody`): its emission sees only the callee of
/// that file, not the file's other `when`s that number the tables.
#[test]
fn another_files_inline_function_keeps_its_own_tables_like_kotlinc_and_runs() {
    let (files, java) = krusty::conformance::split_files(CROSS_FILE).expect("// FILE: blocks");
    assert!(java.is_empty());
    let [(declaration_stem, declaration), (caller_stem, caller)] = files.as_slice() else {
        panic!("expected two Kotlin files, found {}", files.len());
    };
    assert_eq!(
        (declaration_stem.as_str(), caller_stem.as_str()),
        ("Decl", "Caller")
    );
    let sources = [
        ("Decl.kt", declaration.as_str()),
        ("Caller.kt", caller.as_str()),
    ];
    let classes = common::classes_against_kotlinc_module(&sources);
    for name in ["DeclKt", "DeclKt$WhenMappings"] {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
    assert_runs_like_kotlinc(&sources, "CallerKt");
}

#[test]
fn a_when_only_a_top_level_default_wrapper_evaluates_is_numbered_like_kotlinc() {
    assert_like_kotlinc(&[("Pick.kt", DEFAULT_WRAPPER)]);
}

#[test]
fn a_when_only_a_top_level_default_wrapper_evaluates_runs() {
    assert_runs_like_kotlinc(&[("Pick.kt", DEFAULT_WRAPPER)], "PickKt");
}

#[test]
fn a_when_only_a_public_inline_members_default_wrapper_evaluates_is_numbered_like_kotlinc() {
    assert_like_kotlinc(&[("Picker.kt", MEMBER_DEFAULT)]);
}

#[test]
fn a_when_only_a_public_inline_members_default_wrapper_evaluates_runs() {
    assert_runs_like_kotlinc(&[("Picker.kt", MEMBER_DEFAULT)], "PickerKt");
}
