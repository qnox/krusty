//! A `disable`-compiled interface member is still a virtual call.
//!
//! kotlin-stdlib's `ClosedRange.contains` is abstract on the interface and implemented by
//! `ClosedRange$DefaultImpls`. An ordinary call must `invokeinterface` so an override runs.
//! A class that does not override still forwards to the holder, and `super.contains` does too.

use super::common;

#[test]
fn an_override_of_closed_range_contains_runs() {
    const SRC: &str = "\
class Plain(override val start: String, override val endInclusive: String) : ClosedRange<String>\n\
class Custom(override val start: String, override val endInclusive: String) : ClosedRange<String> {\n\
    override fun contains(value: String): Boolean = value == \"hit\"\n\
}\n\
class ViaSuper(override val start: String, override val endInclusive: String) : ClosedRange<String> {\n\
    override fun contains(value: String): Boolean = false\n\
    fun throughSuper(value: String): Boolean = super.contains(value)\n\
}\n\
fun box(): String {\n\
    val plain: ClosedRange<String> = Plain(\"a\", \"c\")\n\
    if (\"b\" !in plain) return \"plain-miss\"\n\
    if (\"d\" in plain) return \"plain-hit\"\n\
    val custom: ClosedRange<String> = Custom(\"a\", \"c\")\n\
    if (!custom.contains(\"hit\")) return \"custom-miss\"\n\
    if (custom.contains(\"b\")) return \"custom-default\"\n\
    val via = ViaSuper(\"a\", \"c\")\n\
    if (via.contains(\"b\")) return \"override-ran\"\n\
    if (!via.throughSuper(\"b\")) return \"super-miss\"\n\
    if (via.throughSuper(\"d\")) return \"super-outside\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "InterfaceHolderDispatch");
}
