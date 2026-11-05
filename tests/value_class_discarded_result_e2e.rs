//! A value class read out of a generic slot and then discarded is popped as the slot holds it.
//!
//! kotlinc's codegen discards an expression statement's value without coercing it
//! (`PromisedValue.discard` pops the callee's own result type): `id(ic)` as a statement is
//! `invokestatic id; pop`, with neither the `checkcast` to the box nor `unbox-impl`. krusty unboxed
//! the discarded result first. A value the statement goes on to use is still unboxed.

use super::common;

const SOURCE: &str = "@JvmInline value class Foo(val value: Int)\n\
    @JvmInline value class Name(val text: String)\n\
    @JvmInline value class Slot(private val holder: Any?) {\n\
    \x20   val size: Int get() = holder as Int\n\
    }\n\
    fun <T> id(x: T): T = x\n\
    fun <T> T.idExtension(): T = this\n\
    fun discard(foo: Foo, name: Name) {\n\
    \x20   id(foo)\n\
    \x20   foo.idExtension()\n\
    \x20   id(foo).idExtension()\n\
    \x20   id(name)\n\
    }\n\
    fun replace(slots: ArrayList<Slot>): Int {\n\
    \x20   slots[0] = Slot(2)\n\
    \x20   return slots[0].size\n\
    }\n\
    fun use(foo: Foo): Int {\n\
    \x20   val kept = id(foo)\n\
    \x20   return kept.value\n\
    }\n\
    fun box(): String {\n\
    \x20   discard(Foo(1), Name(\"a\"))\n\
    \x20   if (replace(arrayListOf(Slot(1))) != 2) return \"fail replace\"\n\
    \x20   if (use(Foo(3)) != 3) return \"fail use\"\n\
    \x20   return \"OK\"\n\
    }\n";

/// Each discarded generic result is popped boxed; the kept one is unboxed.
#[test]
fn a_discarded_value_class_result_is_popped_as_the_slot_holds_it() {
    common::assert_class_matches_kotlinc(
        "ValueClassDiscardedResult",
        SOURCE,
        "ValueClassDiscardedResultKt",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "ValueClassDiscardedResultRun");
}
