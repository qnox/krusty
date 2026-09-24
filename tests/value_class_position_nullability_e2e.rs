//! A parameter or return declared as a NON-null value class is `@NotNull`, as kotlinc annotates it,
//! even when the value class's carrier admits null (`value class Slot(val raw: Any?)` erases to
//! `Object`, `Count(val raw: Int?)` to `Integer`). kotlinc reads the declared type; krusty read the
//! erased carrier and wrote `@Nullable`.
use super::common;

#[test]
fn non_null_value_class_positions_are_not_null_like_kotlinc() {
    const SOURCE: &str = "\
@JvmInline
value class Slot(val raw: Any?)

@JvmInline
value class Label(val raw: String?)

@JvmInline
value class Count(val raw: Int?)

fun slotRaw(value: Slot): Any? = value.raw

fun labelRaw(value: Label): String? = value.raw

fun countRaw(value: Count): Int? = value.raw

fun slotOf(raw: Any?): Slot = Slot(raw)

fun labelOf(raw: String?): Label = Label(raw)
";
    match common::byte_diff_against_kotlinc_cp(
        "ValueClassPositions",
        SOURCE,
        "ValueClassPositionsKt",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (ValueClassPositions: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(why)) => panic!("{why}"),
    }
}
