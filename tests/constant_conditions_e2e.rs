//! kotlinc's `ConstantConditionEliminationMethodTransformer`: an `int` jump whose operands are
//! constants, directly or through locals, becomes a `goto` or goes, and the branch it no longer
//! takes goes with it.
//!
//! krusty kept `iload; ifeq` on an inline function's parameter bound to a constant argument, and a
//! comparison of two locals holding constants.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};
use super::temporary_elimination_e2e::stack_map;

const SOURCE: &str = "inline fun pick(b: Boolean): Int = if (b) 1 else 2\n\
    inline fun sign(x: Int): Int = if (x > 0) 1 else if (x < 0) -1 else 0\n\
    inline fun above(x: Int, limit: Int): Boolean = x > limit\n\
    fun constCond(): Int = pick(true)\n\
    fun constCondFalse(): Int = pick(false)\n\
    fun signOfMinus(): Int = sign(-200)\n\
    fun aboveZero(x: Int): Int = if (above(x, 0)) 1 else 2\n\
    fun localConsts(): Int {\n\
    \x20   val a = 3\n\
    \x20   val b = 40000\n\
    \x20   return if (a < b) a else b\n\
    }\n\
    fun localFlag(): Int {\n\
    \x20   val on = true\n\
    \x20   return if (on) 10 else 20\n\
    }\n\
    fun countToThree(): Int {\n\
    \x20   var i = 0\n\
    \x20   var s = 0\n\
    \x20   while (i < 3) {\n\
    \x20       s += i\n\
    \x20       i++\n\
    \x20   }\n\
    \x20   return s\n\
    }\n";

fn rows(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|row| row.to_string()).collect()
}

#[test]
fn a_jump_on_constants_folds_like_kotlincs() {
    let built = compare_with_kotlinc_plugin(
        "ConstantConditions",
        SOURCE,
        "ConstantConditionsKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are required");
    // Methods whose whole body, frames included, is kotlinc's. `countToThree` does not fold: its
    // loop head meets `i = 0` with `i + 1`, which is no constant.
    for member in ["int localConsts()", "int localFlag()", "int countToThree()"] {
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
    assert_eq!(
        method_instructions(&built.reference, "int localConsts()"),
        rows(&[
            "0: iconst_3",
            "1: istore_0",
            "2: ldc # // int 40000",
            "4: istore_1",
            "5: iload_0",
            "6: ireturn",
        ])
    );
    // The inline calls fold the same way. kotlinc also keeps its `$i$f$` marker local per inlined
    // call (`iconst_0; istore`), which krusty's inliner does not write yet.
    for (member, reference, krusty) in [
        (
            "int constCond()",
            &[
                "0: iconst_1",
                "1: istore_0",
                "2: iconst_0",
                "3: istore_1",
                "4: iconst_1",
                "5: ireturn",
            ][..],
            &["0: iconst_1", "1: istore_0", "2: iconst_1", "3: ireturn"][..],
        ),
        (
            "int constCondFalse()",
            &[
                "0: iconst_0",
                "1: istore_0",
                "2: iconst_0",
                "3: istore_1",
                "4: iconst_2",
                "5: ireturn",
            ][..],
            &["0: iconst_0", "1: istore_0", "2: iconst_2", "3: ireturn"][..],
        ),
        (
            "int signOfMinus()",
            &[
                "0: sipush -200",
                "3: istore_0",
                "4: iconst_0",
                "5: istore_1",
                "6: iconst_m1",
                "7: ireturn",
            ][..],
            &[
                "0: sipush -200",
                "3: istore_0",
                "4: iconst_m1",
                "5: ireturn",
            ][..],
        ),
        // `x > limit` with `limit` bound to `0` compares `x` with zero (`ifle`), and the popped
        // `limit` load goes in a later pass.
        (
            "int aboveZero(int)",
            &[
                "0: iload_0",
                "1: istore_1",
                "2: iconst_0",
                "3: istore_2",
                "4: iconst_0",
                "5: istore_3",
                "6: iload_1",
                "7: ifle 14",
                "10: iconst_1",
                "11: goto 15",
                "14: iconst_0",
                "15: ifeq 22",
                "18: iconst_1",
                "19: goto 23",
                "22: iconst_2",
                "23: ireturn",
            ][..],
            &[
                "0: iload_0",
                "1: istore_1",
                "2: iconst_0",
                "3: istore_2",
                "4: iload_1",
                "5: ifle 12",
                "8: iconst_1",
                "9: goto 13",
                "12: iconst_0",
                "13: ifeq 20",
                "16: iconst_1",
                "17: goto 21",
                "20: iconst_2",
                "21: ireturn",
            ][..],
        ),
    ] {
        assert_eq!(
            method_instructions(&built.reference, member),
            rows(reference),
            "{member} (kotlinc)"
        );
        assert_eq!(
            method_instructions(&built.krusty, member),
            rows(krusty),
            "{member}"
        );
    }
}

/// Folding changes nothing that runs: each sample returns what it returned before.
#[test]
fn folded_constant_conditions_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (constCond() != 1) return \"constCond\"\n\
             \x20   if (constCondFalse() != 2) return \"constCondFalse\"\n\
             \x20   if (signOfMinus() != -1) return \"signOfMinus\"\n\
             \x20   if (aboveZero(5) != 1) return \"aboveZero 5\"\n\
             \x20   if (aboveZero(0) != 2) return \"aboveZero 0\"\n\
             \x20   if (localConsts() != 3) return \"localConsts\"\n\
             \x20   if (localFlag() != 10) return \"localFlag\"\n\
             \x20   return if (countToThree() == 3) \"OK\" else \"countToThree\"\n\
             }}\n"
        ),
        "ConstantConditionsBox",
    );
}
