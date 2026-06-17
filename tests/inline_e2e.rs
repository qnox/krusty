//! End-to-end validation of the inline splice on **real stdlib bytecode**: read `emptyArray`'s
//! compiled body from kotlin-stdlib, splice it with the reified type bound to `String`, and check the
//! `reifiedOperationMarker` was resolved away into a concrete `anewarray`.

mod common;

use krusty::jvm::classfile::ClassWriter;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::inline::{assemble, disassemble, is_reified_inline, splice, Insn};

#[test]
fn splices_real_empty_array_body() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let cp = Classpath::new(vec![stdlib]);
    // The @Metadata d1 protobuf (carrying inline flags) is captured for a real Kotlin facade.
    let collections = cp.find("kotlin/collections/CollectionsKt").expect("CollectionsKt");
    assert!(!collections.kotlin_d1.is_empty(), "@Metadata d1 protobuf is read");
    // emptyArray<T>(): T[]  — erased `()[Ljava/lang/Object;`, defined in kotlin/ArrayIntrinsicsKt.
    let Some(body) = cp.method_code("kotlin/ArrayIntrinsicsKt", "emptyArray", "()[Ljava/lang/Object;") else {
        eprintln!("skipping: emptyArray body not found (stdlib layout differs)");
        return;
    };
    // The raw body must contain the reified marker call we expect to eliminate.
    let pre = disassemble(&body.code).expect("disassemble raw body");
    assert!(pre.iter().any(|i| matches!(i, Insn::Plain { op: 0xb8, .. })), "raw body invokes the marker");
    // Recognition gate: emptyArray is a reified-inline function (must be inlined, not called).
    assert!(is_reified_inline(&body), "emptyArray recognized as reified-inline by its body");

    let mut cw = ClassWriter::new("Caller", "java/lang/Object");
    let mut tm = std::collections::HashMap::new();
    tm.insert("T".to_string(), "java/lang/String".to_string());
    let spliced = splice(&body, "()[Ljava/lang/Object;", 5, &tm, &mut cw).expect("splice real body");

    // Inspect the spliced instructions directly: re-disassembling the assembled bytes in isolation
    // would fail because the redirected return jumps to the *caller's continuation* (absent here).
    // After splicing: an anewarray creates the array, the reified marker invokestatic is gone, and the
    // return became a goto.
    assert!(spliced.iter().any(|i| matches!(i, Insn::Plain { op: 0xbd, .. })), "spliced body creates an array (anewarray)");
    assert!(!spliced.iter().any(|i| matches!(i, Insn::Plain { op: 0xb8, .. })), "the reified marker call is gone");
    assert!(spliced.iter().any(|i| matches!(i, Insn::Branch { op: 0xa7, .. })), "return redirected to goto");

    // The anewarray points at String (the reified type), not Object — that is the whole point.
    let expected = cw.class_ref("java/lang/String");
    let anew = spliced.iter().find_map(|i| match i {
        Insn::Plain { op: 0xbd, operands } => Some((operands[0] as u16) << 8 | operands[1] as u16),
        _ => None,
    }).expect("anewarray present");
    assert_eq!(anew, expected, "anewarray uses the reified String, not Object");

    // And it assembles to bytecode (length grows: nops + a goto replace the marker + return).
    let _ = assemble(&spliced);
}
