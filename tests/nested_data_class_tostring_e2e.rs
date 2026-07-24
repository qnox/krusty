//! A NESTED data class names itself by its INNERMOST simple name in `toString`, as kotlinc does:
//! `sealed class S { data class P(val n: Int) }` renders `P(n=3)`, not `S.P(n=3)`.
//!
//! The synthesized `toString` built its prefix from the class's internal name with `'$'` replaced by
//! `'.'`, so a hoisted nested class (`pkg/S$P`) kept its outer prefix. Verified against kotlinc, which
//! prints `P(n=3)`. A top-level data class is unaffected (no `$` to split on).

use super::common;

#[test]
fn nested_data_class_tostring_uses_innermost_simple_name() {
    let src = "sealed class S {\n\
        \x20   data class P(val n: Int) : S()\n\
        \x20   data class Q(val a: Int, val b: String) : S()\n\
        }\n\
        data class Top(val n: Int)\n\
        fun box(): String {\n\
        \x20   if (S.P(3).toString() != \"P(n=3)\") return S.P(3).toString()\n\
        \x20   if (S.Q(1, \"x\").toString() != \"Q(a=1, b=x)\") return S.Q(1, \"x\").toString()\n\
        \x20   if (Top(1).toString() != \"Top(n=1)\") return Top(1).toString()\n\
        \x20   return \"OK\"\n\
        }\n";
    common::assert_box_ok_with_stdlib(src, "NestedToString");
}
