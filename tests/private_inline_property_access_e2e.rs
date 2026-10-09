//! A private property read from an inline function that is copied out of its class.
//!
//! kotlinc rewrites that read to `access$get<X>$p` (and a write to `access$set<X>$p`) in the
//! inline template. A private `inline` function expanded into a non-private inline function
//! carries the same calls in its own method, and so does a private function it calls. A private
//! inline function expanded only into a non-inline caller keeps the direct field load.

use super::common;

const SOURCE: &str = "\
private fun hidden(): String = \"h\"\n\
private val secret: String = \"s\"\n\
private inline fun wrap(): String = hidden() + secret\n\
internal inline fun pub(): String = wrap()\n\
fun use(): String = pub()\n\
private inline fun only(): String = hidden() + secret\n\
fun direct(): String = only()\n\
\n\
class C {\n\
    private val secret: String = \"s\"\n\
    private var slot: String = \"s\"\n\
    private inline fun wrap(): String = secret\n\
    internal inline fun pub(): String = wrap()\n\
    fun use(): String = pub()\n\
    private inline fun only(): String = secret\n\
    fun direct(): String = only()\n\
    private inline fun write(): String {\n\
        slot = \"t\"\n\
        return slot\n\
    }\n\
    internal inline fun pubWrite(): String = write()\n\
}\n\
";

#[test]
fn an_exported_inline_template_reads_a_private_property_through_an_accessor() {
    let sources = [("PrivateInlineProperty.kt", SOURCE)];
    let facade = common::ModuleClassPair::compile(&sources, "PrivateInlinePropertyKt");
    for method in [
        "wrap",
        "pub",
        "use",
        "only",
        "direct",
        "access$hidden",
        "access$getSecret$p",
    ] {
        let (reference, krusty) = facade.method_code("PrivateInlinePropertyKt", method);
        assert_eq!(krusty, reference, "PrivateInlinePropertyKt.{method}");
    }
    let class = common::ModuleClassPair::compile(&sources, "C");
    for method in [
        "wrap",
        "pub$main",
        "use",
        "only",
        "direct",
        "write",
        "pubWrite$main",
        "access$getSecret$p",
        "access$setSlot$p",
        "access$getSlot$p",
    ] {
        let (reference, krusty) = class.method_code("C", method);
        assert_eq!(krusty, reference, "C.{method}");
    }
}

const DEFAULT_SOURCE: &str = "\
private val secret: String = \"s\"\n\
private inline fun only(extra: String = \"e\"): String = extra + secret\n\
fun direct(): String = only()\n\
\n\
private inline fun wrap(block: () -> String = { secret }): String = block() + secret\n\
internal inline fun pub(): String = wrap()\n\
fun use(): String = pub()\n\
\n\
class C {\n\
    private val secret: String = \"s\"\n\
    private inline fun only(extra: String = \"e\"): String = extra + secret\n\
    fun direct(): String = only(\"x\")\n\
}\n\
";

#[test]
fn a_private_inline_function_with_a_default_reads_a_private_property_through_an_accessor() {
    let sources = [("PrivateInlinePropertyDefault.kt", DEFAULT_SOURCE)];
    let facade = common::ModuleClassPair::compile(&sources, "PrivateInlinePropertyDefaultKt");
    for method in ["only", "wrap", "wrap$default"] {
        let (reference, krusty) = facade.method_code("PrivateInlinePropertyDefaultKt", method);
        assert_eq!(krusty, reference, "PrivateInlinePropertyDefaultKt.{method}");
    }
    let class = common::ModuleClassPair::compile(&sources, "C");
    let (reference, krusty) = class.method_code("C", "only");
    assert_eq!(krusty, reference, "C.only");
}
