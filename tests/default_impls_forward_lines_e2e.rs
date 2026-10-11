//! Under `-jvm-default=enable` an interface's own bodied member keeps a `$DefaultImpls` forward to
//! its `access$…$jd` bridge. kotlinc starts that forward on the member's declaration line and
//! returns on the line of the body's last token, block or expression body, getter and setter alike.
//! krusty started a wrapped expression body's forward on the body's line and returned on the line
//! it started on.
use super::common;

#[test]
fn a_default_impls_forward_spans_its_members_declaration_lines() {
    let src = "package fwd\n\
               \n\
               var result = \"\"\n\
               \n\
               interface Trait {\n\
               \x20   var property: String\n\
               \x20   var counter: Int\n\
               \x20       get() = 1\n\
               \x20       set(value) {\n\
               \x20           result += value\n\
               \x20       }\n\
               \x20   fun record() {\n\
               \x20       result += property\n\
               \x20   }\n\
               \x20   fun inline() = record()\n\
               \x20   fun value(): String {\n\
               \x20       return property\n\
               \x20   }\n\
               \x20   fun empty() {\n\
               \x20   }\n\
               \x20   fun wrapped(): String =\n\
               \x20       \"a\" +\n\
               \x20           \"b\"\n\
               \x20   fun wrappedUnit() =\n\
               \x20       record(\n\
               \x20       )\n\
               \x20   val block: String\n\
               \x20       get() {\n\
               \x20           return \"p\"\n\
               \x20       }\n\
               }\n\
               \n\
               class Impl(override var property: String) : Trait\n\
               \n\
               fun box(): String {\n\
               \x20   val impl = Impl(\"O\")\n\
               \x20   impl.record()\n\
               \x20   impl.counter = 2\n\
               \x20   impl.empty()\n\
               \x20   return if (result == \"O2\" && impl.value() == \"O\") \"OK\" else result\n\
               }\n";
    common::byte_diff_against_kotlinc_cp(
        "DefaultImplsForwardCloseLine",
        src,
        "fwd/Trait$DefaultImpls",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("fwd/Trait$DefaultImpls byte-identical to kotlinc");
}
