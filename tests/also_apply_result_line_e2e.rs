//! An inlined `return this` (`also`/`apply`, or a lambda whose value is a local read) closes the
//! spliced lambda before that read. A multi-line `}` is a `nop` while the lambda locals are still
//! open; a one-line brace shares the body's line and the sweep drops the `nop`. A discarded
//! statement keeps a second `nop` on the call's line after those locals end.
use super::common;

#[test]
fn an_inlined_return_this_matches_kotlinc() {
    let src = "package scope\n\
               \n\
               fun kept(): String {\n\
               \x20   val entry = \"dir\".also { it.length }\n\
               \x20   return entry\n\
               }\n\
               \n\
               fun wrapped(): String {\n\
               \x20   val entry = \"dir\".also {\n\
               \x20       it.length\n\
               \x20   }\n\
               \x20   return entry\n\
               }\n\
               \n\
               fun chosen(flag: Boolean): String {\n\
               \x20   val entry = if (flag) {\n\
               \x20       \"dir\".also { it.length }\n\
               \x20   } else {\n\
               \x20       \"file\".also { it.length }\n\
               \x20   }\n\
               \x20   return entry\n\
               }\n\
               \n\
               fun applied(): String {\n\
               \x20   val entry = \"dir\".apply { length }\n\
               \x20   return entry\n\
               }\n\
               \n\
               fun echoed(): String {\n\
               \x20   val entry = \"dir\".let { it }\n\
               \x20   return entry\n\
               }\n\
               \n\
               fun measured(): Int {\n\
               \x20   val n = \"dir\".let { it.length }\n\
               \x20   return n\n\
               }\n\
               \n\
               fun taken(): String {\n\
               \x20   val entry = \"dir\".run { this }\n\
               \x20   return entry\n\
               }\n\
               \n\
               fun dropped() {\n\
               \x20   \"dir\".also { it.length }\n\
               }\n\
               \n\
               fun droppedWrapped() {\n\
               \x20   \"dir\".also {\n\
               \x20       it.length\n\
               \x20   }\n\
               }\n";
    common::byte_diff_against_kotlinc_cp(
        "AlsoApplyResultLine",
        src,
        "scope/AlsoApplyResultLineKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("scope/AlsoApplyResultLineKt byte-identical to kotlinc");
}
