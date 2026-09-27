//! kotlinc's `setExtraLineNumberForVoidReturningFunction`: a body that falls off its end marks its
//! closing `}` line before the implicit return. A block-bodied local function and a lambda get it
//! as a declared function does; for a lambda the line precedes the `Unit` it returns.
use super::common;

#[test]
fn fallthrough_returns_are_byte_identical_to_kotlinc() {
    let src = "package store\n\
               \n\
               fun sink(x: Int) {}\n\
               \n\
               fun local(a: Int) {\n\
               \x20   fun inner(b: Int) {\n\
               \x20       sink(b)\n\
               \x20   }\n\
               \x20   inner(a)\n\
               }\n\
               \n\
               fun lambda(): (Int) -> Unit = { a ->\n\
               \x20   sink(a)\n\
               }\n\
               \n\
               fun endsWithIf(a: Int) {\n\
               \x20   if (a > 0) {\n\
               \x20       sink(a)\n\
               \x20   }\n\
               }\n";
    common::byte_diff_against_kotlinc_cp(
        "FallthroughCloseLine",
        src,
        "store/FallthroughCloseLineKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("store/FallthroughCloseLineKt byte-identical to kotlinc");
}
