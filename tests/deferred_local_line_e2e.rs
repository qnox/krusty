//! A local declared without an initializer (`val task: String`) is given its type's default value
//! by the parser. That value is written nowhere, so it belongs to the declaration's line: anchoring
//! it at the token that starts the next line gave the method extra `LineNumberTable` entries for
//! the blank and following lines, which kotlinc does not emit.

use super::common;

#[test]
fn a_deferred_local_records_no_line_beyond_its_declaration() {
    // PLATFORM-SPECIFIC: compare JVM LineNumberTable and complete classfile bytes. The whole
    // portable program is also executed by the shared box inventory.
    let src = include_str!("box_regressions/deferredLocalLine.kt");
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
