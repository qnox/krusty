//! A dependency value class constructed through its `constructor-impl$default` stub, and a `super`
//! call into a dependency interface's default body, pass their value-class operands against the
//! selected declaration's parameters.
//!
//! PLATFORM-SPECIFIC: `constructor-impl$default` is a JVM static stub, and the dependency interface
//! body is reached through the JVM's `super` realization. The Wasm and Native KLIB lanes also still
//! decline both shapes ("a constructor call with omitted arguments", "a `super` call to a method of
//! `Sink`, which is not declared in this file"); those backend gaps are not recorded as exceptions.
use super::common;

const DEFAULTS_LIB: &str = "@JvmInline\n\
    value class Id(val raw: Int)\n\
    \n\
    @JvmInline\n\
    value class Level(val id: Id = Id(9))\n\
    \n\
    @JvmInline\n\
    value class Tag(val label: String = \"base\")\n";

const DEFAULTS_MAIN: &str = "fun box(): String {\n\
        if (Level() != Level(Id(9))) return \"default value class\"\n\
        if (Level(Id(2)) == Level()) return \"supplied value class\"\n\
        if (Tag() != Tag(\"base\")) return \"default carrier\"\n\
        if (Tag(\"x\") == Tag()) return \"supplied carrier\"\n\
        return \"OK\"\n\
    }\n";

const SUPER_LIB: &str = "@JvmInline\n\
    value class Id(val raw: Int)\n\
    \n\
    interface Sink<T> {\n\
        fun accept(value: T): String = \"sink:$value\"\n\
    }\n";

const SUPER_MAIN: &str = "class Taker : Sink<Id> {\n\
        override fun accept(value: Id): String = \"taker+\" + super.accept(value)\n\
    }\n\
    \n\
    fun box(): String {\n\
        val accepted = Taker().accept(Id(2))\n\
        if (accepted != \"taker+sink:Id(raw=2)\") return \"interface super: $accepted\"\n\
        return \"OK\"\n\
    }\n";

#[test]
fn a_dependency_value_class_constructed_with_defaults_adapts_its_arguments() {
    common::expect_box_ok_against(
        "value_class_dependency_construction_defaults",
        DEFAULTS_LIB,
        DEFAULTS_MAIN,
    );
}

#[test]
fn a_super_call_into_a_dependency_interface_boxes_a_value_class_argument() {
    common::expect_box_ok_against("super_call_dependency_interface", SUPER_LIB, SUPER_MAIN);
}
