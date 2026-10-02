//! A reified `as? T` specialized to a primitive yields the boxed nullable wrapper.
//!
//! `safecast<Int>("abc")` is `null`. The success cast of `as? T` has to stay `T?`: specializing
//! it to bare `Int` unboxes the match and then unboxes that `null`.

use super::common;

const SOURCE: &str = "\
inline fun <reified T> safecast(x: Any?): T? = x as? T\n\
\n\
fun box(): String {\n\
    val text = safecast<String>(\"abc\")\n\
    if (text != \"abc\") return \"fail string\"\n\
    val number = safecast<Int>(1)\n\
    if (number != 1) return \"fail int\"\n\
    val mismatch = safecast<Int>(\"abc\")\n\
    if (mismatch != null) return \"fail mismatch\"\n\
    val wide = safecast<Long>(\"abc\")\n\
    if (wide != null) return \"fail long\"\n\
    if ((\"abc\" as? Int) != null) return \"fail direct\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn a_reified_primitive_safe_cast_returns_null_on_a_mismatch() {
    common::expect_box_same_as_kotlinc(SOURCE, "reified primitive safe cast");
}
