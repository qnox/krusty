//! Overload specificity compares an EXTENSION's declared receiver, not only its value parameters.
//!
//! Kotlin's tiebreaker prefers a non-generic candidate once neither is more specific by types. That
//! comparison looked at value parameters alone, so two extensions on the SAME receiver that differ
//! only in a lambda's shape read as incomparable and the call was reported ambiguous — the shape a
//! library takes when it adds a typed overload beside a plain one:
//!
//! ```text
//! fun Node.act(path: String, body: Ctx.() -> Unit): Node
//! fun <R : Any> Node.act(path: String, body: Ctx.(R) -> Unit): Node   // @JvmName
//! ```
//!
//! Excluding the receiver from the comparison was load-bearing the other way, which is why it had
//! been kept out: `fun <T> T.pick() where T : Comparable<T>, T : Named` and `fun Any.pick()` take no
//! value parameter at all, so only the receiver distinguishes them, and dropping the generic one
//! there picks the wrong function. Comparing the receiver alongside the parameters serves both.

use super::common;

/// The ambiguous shape: a plain and a typed overload on the same receiver, selected by a lambda
/// written with no parameters.
#[test]
fn a_typed_overload_beside_a_plain_one_is_not_ambiguous() {
    const LIB: &str = "package dep\n\
class Ctx\n\
class Node\n\
fun Node.act(path: String, body: Ctx.() -> Unit): Node = this\n\
@JvmName(\"actTypedPath\")\n\
inline fun <reified R : Any> Node.act(path: String, noinline body: Ctx.(R) -> Unit): Node = this\n\
fun Node.act(body: Ctx.() -> Unit): Node = this\n\
@JvmName(\"actTyped\")\n\
inline fun <reified R : Any> Node.act(noinline body: Ctx.(R) -> Unit): Node = this\n";
    const MAIN: &str = "import dep.*\n\
fun box(): String {\n\
\x20   var seen = \"\"\n\
\x20   Node().act(\"/a\") { seen += \"x\" }\n\
\x20   Node().act { seen += \"y\" }\n\
\x20   return if (seen == \"\") \"OK\" else \"F:$seen\"\n\
}\n";
    let Some(out) = common::expect_box_run_against_ref("extension_specificity_typed", LIB, MAIN)
    else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}

/// The receiver still decides when it is the ONLY position that differs: the generic candidate's
/// bounded receiver is strictly more specific than `Any`, so it must survive the tiebreaker.
#[test]
fn a_bounded_generic_receiver_outranks_any() {
    const LIB: &str = "package dep\n\
interface Named\n\
fun <T> T.pick(): String where T : Comparable<T>, T : Named = \"bounded\"\n\
fun Any.pick(): String = \"fallback\"\n";
    const MAIN: &str = "import dep.*\n\
class Good : Comparable<Good>, Named {\n\
\x20   override fun compareTo(other: Good): Int = 0\n\
}\n\
class Bad : Comparable<Bad> {\n\
\x20   override fun compareTo(other: Bad): Int = 0\n\
}\n\
fun box(): String {\n\
\x20   if (Good().pick() != \"bounded\") return \"good\"\n\
\x20   if (Bad().pick() != \"fallback\") return \"bad\"\n\
\x20   return \"OK\"\n\
}\n";
    let Some(out) = common::expect_box_run_against_ref("extension_specificity_bounds", LIB, MAIN)
    else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}
