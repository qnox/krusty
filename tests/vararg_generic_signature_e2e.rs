//! A `vararg` parameter's generic `Signature`: its element is a value parameter, so declaration-site
//! variance is written on the element's arguments, as for any other parameter. kotlinc signs
//! `vararg pairs: Pair<String, Any?>` as `[Lkotlin/Pair<Ljava/lang/String;+Ljava/lang/Object;>;`,
//! while a plain `Array<Pair<String, Any?>>` parameter keeps the element invariant. An extension
//! receiver comes before the vararg among the JVM parameters, and a local function's captured
//! values come before both.

use super::common;

const SOURCE: &str = "class Sh\n\
\n\
fun case(word: String, vararg branches: Pair<String, Sh.() -> Unit>): String =\n\
\x20   word + branches.size\n\
\n\
fun flex(vararg pairs: Pair<String, Any?>): Int = pairs.size\n\
\n\
fun lists(vararg xs: List<String>): Int = xs.size\n\
\n\
fun plain(xs: Array<Pair<String, Any?>>): Int = xs.size\n\
\n\
fun Sh.chain(vararg branches: Pair<String, Sh.() -> Unit>): Int = branches.size\n\
\n\
fun <T> Sh.generic(vararg pairs: Pair<String, List<T>>): Int = pairs.size\n\
\n\
fun outer(captured: Array<Pair<String, Any?>>): Int {\n\
\x20   fun local(vararg values: Pair<String, Any?>): Int = captured.size + values.size\n\
\x20   return local(\"a\" to 1, \"b\" to null)\n\
}\n\
\n\
fun <T> genericOuter(captured: Array<Pair<String, List<T>>>): Int {\n\
\x20   fun local(vararg values: Pair<String, List<T>>): Int = captured.size + values.size\n\
\x20   return local(\"a\" to emptyList())\n\
}\n";

const BOX: &str = "fun box(): String {\n\
\x20   val total = flex(\"a\" to 1, \"b\" to null) + lists(listOf(\"x\")) +\n\
\x20       plain(arrayOf(\"c\" to 2)) + Sh().chain(\"d\" to {}) +\n\
\x20       Sh().generic(\"f\" to listOf(1)) + outer(arrayOf(\"g\" to 3)) +\n\
\x20       genericOuter(arrayOf(\"h\" to listOf(4)))\n\
\x20   return if (case(\"w\", \"e\" to {}) == \"w1\" && total == 11) \"OK\" else \"fail\"\n\
}\n";

#[test]
fn a_vararg_element_keeps_its_declaration_site_wildcards() {
    let classes = common::classes_against_kotlinc_module(&[("Varargs.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn functions_taking_projected_varargs_run() {
    let source = format!("{SOURCE}\n{BOX}");
    common::expect_box_ok_with_stdlib(&source, "vararg generic signature");
}
