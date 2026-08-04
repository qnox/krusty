//! A `fun interface` whose SAM method mentions a `@JvmInline value class` erases that slot to the
//! class's underlying and JVM-name-mangles the method (`accept-<hash>`). A lambda converted to such an
//! interface must realize the DECLARED shape, not the generic `FunctionN.invoke` one: the closure's
//! parameter/return carry the carrier unboxed, and its `invokedynamic` names the mangled method.
//! Getting either wrong fails only at RUN time — a `ClassCastException` on the carrier, or an
//! `AbstractMethodError` because the closure implements a method the interface never declared.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn value_class_sam_parameter_passes_the_unboxed_carrier() {
    const SRC: &str = "@JvmInline\n\
value class Tag(val raw: String)\n\
fun interface TagSink { fun accept(tag: Tag) }\n\
fun feed(sink: TagSink) { sink.accept(Tag(\"fed\")) }\n\
fun box(): String {\n\
    var seen = \"none\"\n\
    feed { t -> seen = t.raw }\n\
    return if (seen == \"fed\") \"OK\" else \"fail: $seen\"\n\
}\n";
    assert_eq!(run(SRC).expect("value-class SAM parameter"), "OK");
}

#[test]
fn value_class_sam_return_hands_back_the_unboxed_carrier() {
    const SRC: &str = "@JvmInline\n\
value class Tag(val raw: String)\n\
fun interface TagSource { fun produce(): Tag }\n\
fun draw(source: TagSource): Tag = source.produce()\n\
fun box(): String {\n\
    val drawn = draw { Tag(\"drawn\") }\n\
    return if (drawn.raw == \"drawn\") \"OK\" else \"fail: ${drawn.raw}\"\n\
}\n";
    assert_eq!(run(SRC).expect("value-class SAM return"), "OK");
}

#[test]
fn scalar_underlying_value_class_sam_round_trips() {
    const SRC: &str = "@JvmInline\n\
value class Celsius(val degrees: Int)\n\
fun interface Thermostat { fun adjust(reading: Celsius): Celsius }\n\
fun apply(t: Thermostat, reading: Celsius): Celsius = t.adjust(reading)\n\
fun box(): String {\n\
    val out = apply({ r -> Celsius(r.degrees + 5) }, Celsius(15))\n\
    return if (out.degrees == 20) \"OK\" else \"fail: ${out.degrees}\"\n\
}\n";
    assert_eq!(run(SRC).expect("scalar-underlying value-class SAM"), "OK");
}

#[test]
fn generic_sam_slot_still_carries_a_boxed_value_class() {
    // The counterpart shape: the SAM method declares a TYPE PARAMETER, so its slot is generic and the
    // value class travels through it BOXED — the same reading a plain `FunctionN` lambda gets. Only a
    // slot the interface spells as the value class itself may carry the raw carrier.
    const SRC: &str = "@JvmInline\n\
value class Tag(val raw: String)\n\
fun interface Holder<T> { fun take(value: T): String }\n\
fun useHolder(h: Holder<Tag>): String = h.take(Tag(\"held\"))\n\
fun box(): String {\n\
    val seen = useHolder { v -> v.raw }\n\
    return if (seen == \"held\") \"OK\" else \"fail: $seen\"\n\
}\n";
    assert_eq!(run(SRC).expect("generic SAM slot keeps the box"), "OK");
}
