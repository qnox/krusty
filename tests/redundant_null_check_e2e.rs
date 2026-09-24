//! kotlinc's redundant null-check elimination, applied to krusty's finished methods.
//!
//! `x as String` checks `x` for `null` before the cast. kotlinc's nullability analysis
//! (`RedundantNullCheckMethodTransformer`) then removes the check where `x` is already known
//! non-null: a parameter past its `checkNotNullParameter`, a local past an `ifnull` of it, a value
//! already checked once. krusty kept every check.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};
use super::temporary_elimination_e2e::stack_map;

const SOURCE: &str = "fun checkedParameter(a: Any): String = a as String\n\
    fun pastItsNullCheck(a: Any?): String {\n\
    \x20   if (a == null) return \"\"\n\
    \x20   return a as String\n\
    }\n\
    fun castTwice(a: Any): Int {\n\
    \x20   val s = a as String\n\
    \x20   val t = a as String\n\
    \x20   return s.length + t.length\n\
    }\n\
    class GenericSource<T>(private val value: T) { fun read(): T = value }\n\
    fun fromACall(source: GenericSource<Any>): String = source.read() as String\n";

#[test]
fn a_null_check_of_a_known_non_null_value_is_dropped_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "RedundantNullCheck",
        SOURCE,
        "RedundantNullCheckKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // `fromACall` keeps its check in both: a generic call's result is not known non-null.
    for member in [
        "java.lang.String checkedParameter(java.lang.Object)",
        "java.lang.String pastItsNullCheck(java.lang.Object)",
        "int castTwice(java.lang.Object)",
        "java.lang.String fromACall(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
        assert_eq!(
            stack_map(&built.krusty, member),
            stack_map(&built.reference, member),
            "{member} frames"
        );
    }
}

#[test]
fn code_without_its_redundant_null_checks_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (checkedParameter(\"a\") != \"a\") return \"parameter\"\n\
             \x20   if (pastItsNullCheck(null) != \"\" || pastItsNullCheck(\"b\") != \"b\") return \"check\"\n\
             \x20   if (castTwice(\"cd\") != 4) return \"twice\"\n\
             \x20   try {{\n\
             \x20       fromACall(GenericSource<Any>(1))\n\
             \x20       return \"cast\"\n\
             \x20   }} catch (e: ClassCastException) {{\n\
             \x20   }}\n\
             \x20   return if (fromACall(GenericSource<Any>(\"e\")) == \"e\") \"OK\" else \"call\"\n\
             }}\n"
        ),
        "redundant null checks",
    );
}
