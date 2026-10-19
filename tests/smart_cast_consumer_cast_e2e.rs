//! A smart cast is cast only to what its consumer reads, as kotlinc does.
//!
//! kotlinc's smart cast is an `IMPLICIT_CAST`, which its JVM codegen emits as the unnarrowed operand
//! (`ExpressionCodegen.visitTypeOperator`); the consumer then materializes that value at its own
//! type (`PromisedValue.materializeAt`, `StackValue.coerce`). An `Object` parameter, a string
//! template's `StringBuilder.append(Object)`, and an erased `T` therefore take the value with no
//! `checkcast`, and a supertype parameter casts to that supertype only. krusty narrowed to the
//! smart-cast type first.
use super::common;

const SOURCE: &str = "interface Marker\n\
    open class Base\n\
    class Leaf : Base(), Marker { override fun toString() = \"Leaf\" }\n\
    fun <T> generic(value: T): String = \"generic\"\n\
    fun anything(value: Any?): String = \"any\"\n\
    fun base(value: Base): String = \"base\"\n\
    fun marker(value: Marker): String = \"marker\"\n\
    fun template(value: Any): String = if (value is Marker) \"is $value\" else \"not\"\n\
    fun toAnything(value: Any): String = if (value is Marker) anything(value) else \"not\"\n\
    fun toGeneric(value: Any): String = if (value is Marker) generic(value) else \"not\"\n\
    fun toBase(value: Any): String = if (value is Leaf) base(value) else \"not\"\n\
    fun toMarker(value: Any): String = if (value is Marker) marker(value) else \"not\"\n";

#[test]
fn a_smart_cast_value_is_cast_only_to_its_consumers_type_like_kotlincs() {
    common::byte_diff_against_kotlinc_cp(
        "SmartCastConsumerCast",
        SOURCE,
        "SmartCastConsumerCastKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("SmartCastConsumerCastKt byte-identical to kotlinc");
}

#[test]
fn a_smart_cast_value_consumed_uncast_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val leaf = Leaf()\n\
             \x20   if (template(leaf) != \"is Leaf\" || toAnything(leaf) != \"any\") return \"any\"\n\
             \x20   if (toGeneric(leaf) != \"generic\" || toBase(leaf) != \"base\") return \"base\"\n\
             \x20   return if (toMarker(leaf) == \"marker\" && toMarker(1) == \"not\") \"OK\" else \"marker\"\n\
             }}\n"
        ),
        "smart cast consumer casts",
    );
}
