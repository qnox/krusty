//! A type check whose target is a primitive type, used as a condition, jumps on the wrapper's
//! `instanceof` as a reference target's check does, without materializing a boolean first.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
fun notInt(x: Any?): String {\n\
    if (x !is Int) return \"a\"\n\
    return \"b\"\n\
}\n\
\n\
fun isLong(x: Any): Int {\n\
    while (x is Long) {\n\
        return 1\n\
    }\n\
    return 0\n\
}\n\
\n\
fun bothChar(x: Any?, y: Any?): Boolean {\n\
    if (x is Char && y !is Double) return true\n\
    return false\n\
}\n\
\n\
fun subject(x: Any?): String {\n\
    if (x !is Int) return \"fail\"\n\
    when (x) {\n\
        0 -> return \"zero\"\n\
        else -> return \"other\"\n\
    }\n\
}\n\
";

#[test]
fn primitive_type_check_conditions_jump_on_instanceof() {
    common::byte_diff_against_kotlinc_cp(
        "PrimitiveTypeCheckCondition",
        SOURCE,
        "store/PrimitiveTypeCheckConditionKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("primitive type check conditions differ from kotlinc: {diff}"));
}
