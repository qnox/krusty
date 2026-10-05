//! An elvis over a safe call keeps its own null check of the safe call's value, as kotlinc's
//! output does: the selector's non-null type does not fuse the two null jumps. Only the bytecode
//! null-check analysis removes the elvis's check, where it proves the value non-null.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Box(val name: String, val n: Int) {\n\
                      \x20   fun label(): String = name\n\
                      }\n\
                      \n\
                      fun call(b: Box?): String = b?.label() ?: \"none\"\n\
                      fun property(b: Box?): String = b?.name ?: \"none\"\n\
                      fun primitive(b: Box?): Int = b?.n ?: -1\n\
                      fun early(b: Box?): String {\n\
                      \x20   val v = b?.label() ?: return \"early\"\n\
                      \x20   return v\n\
                      }\n\
                      fun chain(b: Box?, c: Box): String = b?.label() ?: c.label()\n";

#[test]
fn an_elvis_over_a_safe_call_keeps_its_null_check_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "ElvisOverSafeCall",
        SOURCE,
        "store/ElvisOverSafeCallKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/ElvisOverSafeCallKt differs from kotlinc: {diff}"));
}
