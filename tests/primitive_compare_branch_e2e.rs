//! Kotlin's `<`/`<=`/`>`/`>=` are the `compareTo` operator, so the front end models them as a
//! three-way comparison tested against zero. That result exists only to be tested: kotlinc emits the
//! direct comparison, and krusty emitted the call.
//!
//! ```
//! kotlinc                    krusty (before)
//!   iload_0                    iload_0
//!   iload_1                    iload_1
//!   if_icmpge 9                invokestatic java/lang/Integer.compare:(II)I
//!                              ifge 12
//! ```
//!
//! Every comparison of two primitives in every Kotlin file carried the extra call and its constant
//! pool entry, so this is not a corner of the language — it is one of the most common shapes there
//! is.
use super::common;

/// `Int` fuses into a two-operand branch; `Long` keeps its `lcmp` and tests THAT against zero,
/// which is the same rule seen from the other side — only the int category has a direct form.
#[test]
fn a_primitive_comparison_is_byte_identical_to_kotlinc() {
    let src = "fun lessInt(x: Int, y: Int): Boolean = x < y\n\
               fun chooseInt(x: Int, y: Int): Int = if (x < y) 1 else 2\n\
               fun lessLong(x: Long, y: Long): Boolean = x < y\n\
               fun atLeastDouble(x: Double, y: Double): Boolean = x >= y\n";
    let Some(result) =
        common::byte_diff_against_kotlinc("PrimitiveCompare", src, "PrimitiveCompareKt")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("PrimitiveCompareKt byte-identical to kotlinc");
}

/// An explicit floating `compareTo` uses total ordering, unlike the relational operator's IEEE
/// comparison. The inner call must not be fused merely because its integer result is tested against
/// zero: signed zero and NaN make the two meanings observably different.
#[test]
fn an_explicit_floating_compare_keeps_total_ordering() {
    let src = "fun box(): String {\n\
               \x20   if (!((-0.0).compareTo(0.0) < 0)) return \"fail signed zero\"\n\
               \x20   if (!(Double.NaN.compareTo(0.0) > 0)) return \"fail NaN total order\"\n\
               \x20   if (Double.NaN < 0.0) return \"fail NaN relation\"\n\
               \x20   return \"OK\"\n\
               }\n";
    let Some(actual) = common::compile_and_run_box(
        src,
        "explicit_floating_compare",
        &[common::stdlib_jar()],
        None,
    ) else {
        eprintln!("skipping: JVM runner unavailable");
        return;
    };
    assert_eq!(actual, "OK");
}

/// The comparison still has to MEAN the same thing. A three-way result read with the zero on the
/// left reverses the comparison, and a fusion that dropped that would pass a byte test written only
/// for the ordinary spelling.
#[test]
fn a_fused_comparison_still_orders_correctly() {
    let src = "fun box(): String {\n\
               \x20   val order = listOf(3 to 5, 5 to 3, 4 to 4)\n\
               \x20       .joinToString(\",\") { (a, b) -> \"\" + (a < b) + (a <= b) + (a > b) + (a >= b) }\n\
               \x20   return order\n\
               }\n";
    let Some(actual) =
        common::compile_and_run_box(src, "fused_compare", &[common::stdlib_jar()], None)
    else {
        eprintln!("skipping: JVM runner unavailable");
        return;
    };
    assert_eq!(
        actual, "truetruefalsefalse,falsefalsetruetrue,falsetruefalsetrue",
        "a fused comparison must order exactly as the operator does"
    );
}
