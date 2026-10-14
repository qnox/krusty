//! kotlinc's `MappedEnumWhenLowering`: a subject `when` over an enum reads the entry's ordinal
//! through a `$EnumSwitchMapping$N` table of the innermost class's synthetic `$WhenMappings`
//! nested class, numbering entries in the order the class's `when`s first compare them, so that a
//! dependency adding entries does not change the switch keys.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    enum class E { A, B, C }\n\
    enum class F { X, Y }\n\
    \n\
    fun one(e: E): Int = when (e) { E.B -> 1; else -> 2 }\n\
    fun two(e: E?): Int = when (e) { E.C -> 1; null -> 3; E.A, E.B -> 4 }\n\
    fun stmt(e: E) {\n\
    \x20   when (e) {\n\
    \x20       E.A -> println(\"a\")\n\
    \x20       E.B -> println(\"b\")\n\
    \x20       else -> {}\n\
    \x20   }\n\
    }\n\
    fun nested(e: E, f: F): Int = when (e) {\n\
    \x20   E.A -> when (f) { F.Y -> 5; F.X -> 6 }\n\
    \x20   else -> 7\n\
    }\n\
    \n\
    class K {\n\
    \x20   fun m(f: F) = when (f) { F.Y -> \"y\"; F.X -> \"x\" }\n\
    \x20   fun n(e: E) = when (e) { E.A -> \"x\"; E.B -> \"y\"; E.C -> \"z\" }\n\
    }\n\
    \n\
    fun objectOwned(e: E): String = object {\n\
    \x20   fun select(value: E) = when (value) { E.B -> \"object\"; else -> \"other\" }\n\
    }.select(e)\n";

fn assert_identical(class: &str) {
    common::byte_diff_against_kotlinc_cp(
        "EnumWhenMappings",
        SOURCE,
        class,
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn enum_when_reads_the_facade_mappings_like_kotlinc() {
    assert_identical("store/EnumWhenMappingsKt");
}

#[test]
fn facade_when_mappings_class_is_byte_identical_to_kotlinc() {
    assert_identical("store/EnumWhenMappingsKt$WhenMappings");
}

#[test]
fn class_when_reads_its_own_mappings_like_kotlinc() {
    assert_identical("store/K");
}

#[test]
fn class_when_mappings_class_is_byte_identical_to_kotlinc() {
    assert_identical("store/K$WhenMappings");
}

#[test]
fn object_expression_when_reads_its_own_mappings_like_kotlinc() {
    assert_identical("store/EnumWhenMappingsKt$objectOwned$1");
}

#[test]
fn object_expression_when_mappings_class_is_byte_identical_to_kotlinc() {
    assert_identical("store/EnumWhenMappingsKt$objectOwned$1$WhenMappings");
}

#[test]
fn enum_when_mappings_execute() {
    let out = common::compile_and_run_with_stdlib(
        &format!(
            "{SOURCE}\nfun box(): String {{\n\
             \x20   stmt(E.B)\n\
             \x20   return listOf(one(E.A), one(E.B), two(null), two(E.A), two(E.C)).toString() +\n\
             \x20       listOf(nested(E.A, F.X), nested(E.A, F.Y), nested(E.C, F.X)) +\n\
             \x20       K().m(F.X) + K().n(E.C) + objectOwned(E.B)\n\
             }}\n"
        ),
        "EnumWhenMappingsRun",
    );
    assert_eq!(out.as_deref(), Some("[2, 1, 3, 4, 1][6, 5, 7]xzobject"));
}
