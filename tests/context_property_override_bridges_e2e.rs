//! An override of a property that takes context parameters is reached through its supertype's
//! accessors, which take the contexts before the value. Where the override's erased types differ
//! from the declaration's (a generic `T`, or a covariant result), the class carries a bridge with
//! the declaration's descriptor, and the bridge forwards the context operands. Each program runs
//! under kotlinc and krusty with the same result.
use super::common;

#[test]
fn a_generic_context_property_is_overridden_through_its_supertype() {
    const SOURCE: &str = "// LANGUAGE: +ContextParameters\n\
        abstract class Slots<T> {\n\
        \x20   context(unit: Int)\n\
        \x20   abstract var slot: T\n\
        \x20   context(unit: Int)\n\
        \x20   fun write(value: T) { slot = value }\n\
        \x20   context(unit: Int)\n\
        \x20   fun read(): T = slot\n\
        }\n\
        class TextSlots : Slots<String>() {\n\
        \x20   var stored = \"\"\n\
        \x20   context(unit: Int)\n\
        \x20   override var slot: String\n\
        \x20       get() = stored + unit\n\
        \x20       set(value) { stored = value + \"-\" }\n\
        }\n\
        fun box(): String {\n\
        \x20   val slots: Slots<String> = TextSlots()\n\
        \x20   with(2) {\n\
        \x20       slots.write(\"v\")\n\
        \x20       if (slots.read() != \"v-2\") return \"through the supertype: ${slots.read()}\"\n\
        \x20   }\n\
        \x20   return \"OK\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SOURCE, "context_property_generic_override");
}

#[test]
fn a_covariant_context_property_result_is_bridged_with_its_contexts() {
    const SOURCE: &str = "// LANGUAGE: +ContextParameters\n\
        abstract class Named {\n\
        \x20   context(prefix: String)\n\
        \x20   abstract val label: Any\n\
        \x20   context(prefix: String)\n\
        \x20   fun describe(): String = label.toString()\n\
        }\n\
        class Short : Named() {\n\
        \x20   context(prefix: String)\n\
        \x20   override val label: String get() = prefix + \"!\"\n\
        }\n\
        fun box(): String {\n\
        \x20   val named: Named = Short()\n\
        \x20   with(\"ok\") {\n\
        \x20       if (named.describe() != \"ok!\") return \"covariant: ${named.describe()}\"\n\
        \x20   }\n\
        \x20   return \"OK\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SOURCE, "context_property_covariant_override");
}
