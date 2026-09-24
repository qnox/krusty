//! A `for` loop over an iterable calls `iterator()` on the iterable value, taking no local for it.
//!
//! kotlinc lowers `for (x in xs)` to `val it = xs.iterator(); while (it.hasNext()) …`. krusty first
//! stored the iterable in a temporary and called `iterator()` on that. The bytecode pass folded the
//! one store and load, but the temporary's slot stayed reserved, so the iterator, the loop variable
//! and every later local sat one slot above kotlinc's.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "fun words(): Sequence<String> = sequenceOf(\"a\", \"bb\")\n\
    fun counted(): Int {\n\
    \x20   var sum = 0\n\
    \x20   for (word in words()) {\n\
    \x20       sum += word.length\n\
    \x20   }\n\
    \x20   return sum\n\
    }\n\
    fun listed(xs: List<String>): Int {\n\
    \x20   var n = 0\n\
    \x20   for (x in xs) n += x.length\n\
    \x20   return n\n\
    }\n";

#[test]
fn a_for_loop_iterator_takes_kotlincs_slot() {
    let Some(built) = compare_with_kotlinc_plugin(
        "ForLoopIterableSlot",
        SOURCE,
        "ForLoopIterableSlotKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in ["int counted(", "int listed("] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}
