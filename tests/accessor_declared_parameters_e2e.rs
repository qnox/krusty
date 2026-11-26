//! A module accessor call carries its declaration's parameter vector, so a value-class operand of a
//! context-parameter property accessor in another module is adapted against the declared type.
//!
//! This is a classpath fixture. The Wasm and Native KLIB lanes still decline a property getter call
//! with context arguments across modules ("a property getter call with arguments"); that backend
//! gap is not recorded as an exception here, and the program belongs in `tests/box_regressions/`
//! once they realize it.
use super::common;

const LIB: &str = "// LANGUAGE: +ContextParameters\n\
    @JvmInline\n\
    value class Id(val raw: Int)\n\
    \n\
    abstract class Slots {\n\
        context(unit: Int)\n\
        abstract var slot: Id\n\
    \n\
        context(unit: Int)\n\
        fun readSlot(): Id = slot\n\
    \n\
        context(unit: Int)\n\
        fun writeSlot(value: Id) {\n\
            slot = value\n\
        }\n\
    }\n";

const MAIN: &str = "// LANGUAGE: +ContextParameters\n\
    class IdSlots : Slots() {\n\
        var stored = Id(0)\n\
    \n\
        context(unit: Int)\n\
        override var slot: Id\n\
            get() = Id(stored.raw * unit)\n\
            set(value) {\n\
                stored = Id(value.raw + unit)\n\
            }\n\
    }\n\
    \n\
    fun box(): String {\n\
        val slots = IdSlots()\n\
        with(2) {\n\
            slots.slot = Id(5)\n\
            if (slots.stored != Id(7)) return \"setter: ${slots.stored}\"\n\
            if (slots.slot != Id(14)) return \"getter: ${slots.slot}\"\n\
            slots.writeSlot(Id(1))\n\
            if (slots.stored != Id(3)) return \"writer: ${slots.stored}\"\n\
            if (slots.readSlot() != Id(6)) return \"reader: ${slots.readSlot()}\"\n\
        }\n\
        return \"OK\"\n\
    }\n";

#[test]
fn a_context_accessor_in_another_module_adapts_value_class_operands() {
    common::expect_box_ok_against("context_accessor_value_class_setter", LIB, MAIN);
}
