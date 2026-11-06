//! A local declared without an initializer (`val task: String`) is given its type's default value
//! by the parser. That value is written nowhere, so it belongs to the declaration's line: anchoring
//! it at the token that starts the next line gave the method extra `LineNumberTable` entries for
//! the blank and following lines, which kotlinc does not emit.

use super::common;

#[test]
fn a_deferred_local_records_no_line_beyond_its_declaration() {
    let src = "fun next(): Int = 2\n\
               \n\
               fun box(): String {\n\
               \x20   val task: String\n\
               \n\
               \x20   do {\n\
               \x20       if (next() % 2 == 0) {\n\
               \x20           task = \"OK\"\n\
               \x20           break\n\
               \x20       }\n\
               \x20   }\n\
               \x20   while (true)\n\
               \n\
               \x20   return task\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "DeferredLocalLine",
        src,
        "DeferredLocalLineKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("DeferredLocalLineKt byte-identical to kotlinc");
}
