//! kotlinc's redundant-`checkcast` elimination, applied to krusty's finished methods.
//!
//! kotlinc removes a `checkcast` whose operand is `null` or already exactly the cast's type
//! (`RedundantCheckCastEliminationMethodTransformer`); a local loaded inside its named range counts
//! as its declared type there. krusty's `n?.plus(1)` reloaded its `Integer` temporary through a
//! `checkcast Integer` kotlinc does not write.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};
use super::temporary_elimination_e2e::stack_map;

const SOURCE: &str = "fun next(n: Int?): Int? = n?.plus(1)\n\
    fun <T> same(value: T): T = value\n\
    fun erased(s: String): Int = same(s).length\n";

#[test]
fn a_cast_to_the_type_already_on_the_stack_is_dropped_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "RedundantCheckCast",
        SOURCE,
        "RedundantCheckCastKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // `erased` keeps its cast in both: the erased result on the stack is an `Object`.
    for member in [
        "java.lang.Integer next(java.lang.Integer)",
        "int erased(java.lang.String)",
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
fn code_without_its_redundant_casts_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (next(null) != null || next(1) != 2) return \"next\"\n\
             \x20   return if (erased(\"ab\") == 2) \"OK\" else \"erased\"\n\
             }}\n"
        ),
        "redundant casts",
    );
}
