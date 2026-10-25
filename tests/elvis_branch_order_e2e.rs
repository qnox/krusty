//! kotlinc's `JvmSafeCallChainFoldingLowering.foldElvis` rebuilds every elvis with its left value's
//! branch first: `{ val tmp = lhs; when { tmp != null -> tmp; else -> rhs } }`.

use super::common;

#[test]
fn an_elvis_lays_out_its_left_value_first() {
    let source = "fun boxed(): Int? = 8\n\
         fun name(): String? = \"n\"\n\
         \n\
         fun local(): Int {\n\
         \x20   val grown = boxed()\n\
         \x20   return grown ?: 0\n\
         }\n\
         fun direct(): Int = boxed() ?: 0\n\
         fun summed(): Int {\n\
         \x20   val grown = boxed()\n\
         \x20   return 1 + (grown ?: 0)\n\
         }\n\
         fun str(s: String?): String = s ?: \"d\"\n\
         fun ifForm(): Int {\n\
         \x20   val x = boxed()\n\
         \x20   return if (x == null) 0 else x\n\
         }\n\
         fun lengthElvis(): Int = name()?.length ?: 0\n\
         fun anyElvis(): Any = boxed() ?: \"z\"\n\
         fun longElvis(l: Long?): Long = l ?: 1L\n\
         fun doubleElvis(d: Double?): Double = d ?: 2.0\n\
         fun flag(b: Boolean?): Boolean = b ?: false\n\
         fun returning(i: Int?): Int {\n\
         \x20   val v = i ?: return -1\n\
         \x20   return v * 2\n\
         }\n\
         fun throwing(i: Int?): Int = i ?: throw IllegalStateException(\"none\")\n\
         fun strReturning(s: String?): Int {\n\
         \x20   val v = s ?: return 0\n\
         \x20   return v.length\n\
         }\n\
         fun chained(a: Int?, b: Int?): Int = a ?: b ?: 3\n\
         fun statement(s: String?) {\n\
         \x20   s ?: println(\"missing\")\n\
         }\n\
         fun nullableResult(a: Int?, b: Int?): Int? = a ?: b\n\
         \n\
         fun box(): String {\n\
         \x20   val total = local() + direct() + summed() + str(null).length + ifForm() + lengthElvis() +\n\
         \x20       longElvis(null).toInt() + doubleElvis(null).toInt() + returning(null) + returning(4) +\n\
         \x20       strReturning(\"ab\") + chained(null, null) + (nullableResult(null, 5) ?: 0)\n\
         \x20   statement(null)\n\
         \x20   if (!flag(null) && anyElvis() == 8 && total > 0) return \"OK\"\n\
         \x20   return \"fail\"\n\
         }\n\
         ";
    common::expect_box_ok_with_stdlib(source, "ElvisBranchOrder");
    let classes = common::classes_against_kotlinc_module(&[("ElvisBranchOrder.kt", source)]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
