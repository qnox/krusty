//! A FULLY-QUALIFIED call to a VARARG top-level function (`kotlin.collections.listOf(1, 2, 3)`).
//! A vararg callee packs every trailing argument into ONE array parameter; the fully-qualified path
//! paired arguments with parameters index-for-index, so the first element was measured against
//! `Array<Any>` ("actual type is 'Int', but 'Array<Any>' was expected"), and the lowerer then skipped
//! the shape outright. Both halves are covered — the call type-checks AND runs on a real JVM.
use super::common;

#[test]
fn a_fully_qualified_vararg_call_packs_its_trailing_arguments() {
    let src = "fun box(): String {\n\
        \x20 val xs = kotlin.collections.listOf(1, 2, 3)\n\
        \x20 if (xs.size != 3) return \"size ${xs.size}\"\n\
        \x20 if (xs[2] != 3) return \"elem ${xs[2]}\"\n\
        \x20 val ys = kotlin.collections.listOf(\"a\", \"b\")\n\
        \x20 return if (ys.joinToString(\"\") == \"ab\") \"OK\" else \"ys=$ys\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "FqVarargCall");
}

/// A fully-qualified call to a FIXED-arity overload of the same name keeps binding that overload.
#[test]
fn a_fully_qualified_single_argument_call_keeps_its_fixed_overload() {
    let src = "fun box(): String {\n\
        \x20 val xs = kotlin.collections.listOf(9)\n\
        \x20 return if (xs.size == 1 && xs[0] == 9) \"OK\" else \"xs=$xs\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "FqFixedCall");
}
