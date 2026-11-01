//! A `vararg` parameter is an `Array<out E>`, so its generic `Signature` keeps the element's
//! declaration-site wildcards (`Pair<String, ? extends Function1<? super Sh, Unit>>...`), where an
//! invariant `Array<E>` parameter suppresses them. The vararg is found by its position among the
//! physical parameters: after an extension receiver, before a trailing value parameter, beside a
//! local function's captures, and in front of a suspend function's continuation. Its recorded
//! ordinal counts the declared context parameters ahead of it, and a value class's secondary
//! constructor keeps its vararg on the static `constructor-impl` that realizes it, both in the
//! `Signature` and as `ACC_VARARGS`.

use super::common;

const SOURCE: &str = "\
package va\n\
\n\
class Sh\n\
\n\
@JvmInline\n\
value class Tag(val v: String) {\n\
    constructor(vararg parts: Pair<String, Any?>) : this(parts.joinToString())\n\
    constructor(n: Int, vararg parts: Pair<String, List<Any?>>) : this(parts.joinToString() + n)\n\
    fun join(vararg parts: Pair<String, Any?>): String = v + parts.size\n\
    context(c: String)\n\
    fun scoped(vararg parts: Pair<String, Any?>, tail: Int): String = c + parts.size + tail\n\
}\n\
\n\
class Box<T> {\n\
    fun put(vararg items: Pair<String, T>) {}\n\
    fun all(vararg items: List<Any>) {}\n\
    suspend fun <R> load(vararg keys: List<R>): R? = null\n\
}\n\
\n\
fun case(word: String, vararg branches: Pair<String, Sh.() -> Unit>) {}\n\
fun flex(vararg entries: Pair<String, Any?>) {}\n\
fun array(entries: Array<Pair<String, Any?>>) {}\n\
fun arrayOut(entries: Array<out Pair<String, Any?>>) {}\n\
fun lambdas(vararg blocks: (Sh) -> Unit) {}\n\
fun tagged(vararg tags: Pair<Tag, Any>) {}\n\
fun <T> String.generic(vararg x: Pair<String, T>) {}\n\
fun String.extension(vararg x: Pair<String, Any>) {}\n\
fun middle(vararg x: Pair<String, Any>, tail: Int) {}\n\
suspend fun suspending(vararg x: Pair<String, Any>) {}\n\
\n\
context(c: String)\n\
fun contextual(vararg x: Pair<String, Any?>) {}\n\
context(c: String, d: Sh)\n\
fun contextualTail(vararg x: Pair<String, Any?>, tail: Pair<String, Any?>) {}\n\
context(c: String)\n\
fun Sh.contextualExtension(vararg x: Pair<String, Any?>) {}\n\
\n\
class Holder {\n\
    context(c: String)\n\
    fun member(vararg x: Pair<String, Any?>, tail: Int) {}\n\
    context(c: String)\n\
    fun Sh.memberExtension(vararg x: Pair<String, Any?>) {}\n\
}\n\
\n\
fun outer(seed: Int): Int {\n\
    fun inner(vararg parts: Pair<String, Any>): Int = seed + parts.size\n\
    return inner(\"a\" to 1)\n\
}\n\
";

#[test]
fn a_vararg_parameter_keeps_its_element_wildcards() {
    for class in [
        "va/Box",
        "va/Tag",
        "va/Holder",
        "va/VarargSignatureWildcardsKt",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "VarargSignatureWildcards",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
