//! A value class stored in a generic `T?` slot is there as its box, whatever its carrier. A read of
//! the slot is the erased reference: coerced to a nullable value class whose `X?` is the box, it is
//! narrowed with `checkcast`, never passed through `box-impl` as if it were the carrier; coerced to
//! one whose `X?` is the carrier, the box is unboxed null-safely.

use super::common;

const SRC: &str = "class Slot<T : Any>(val held: T?)\n\
    @JvmInline value class Count(val n: Int)\n\
    @JvmInline value class Text(val string: String)\n\
    fun count(x: Count?) = Slot(x)\n\
    fun text(x: Text?) = Slot(x)\n\
    fun readCount(s: Slot<Count>): Count? = s.held\n\
    fun readText(s: Slot<Text>): Text? = s.held\n\
    fun box(): String {\n\
    \x20   if (readCount(count(null)) != null || readText(text(null)) != null) return \"fail null\"\n\
    \x20   if (readCount(count(Count(1)))?.n != 1) return \"fail count\"\n\
    \x20   return if (readText(text(Text(\"a\")))?.string == \"a\") \"OK\" else \"fail text\"\n\
    }\n";

fn assert_same_instructions(method: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "GenericSlot",
        SRC,
        "GenericSlotKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_generic_slot_read_narrows_a_boxed_nullable_value_class() {
    assert_same_instructions("public static final Count readCount(Slot<Count>)");
}

#[test]
fn a_generic_slot_read_unboxes_a_carrier_nullable_value_class_null_safely() {
    assert_same_instructions("public static final java.lang.String readText(Slot<Text>)");
}

#[test]
fn generic_slot_reads_run() {
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}
