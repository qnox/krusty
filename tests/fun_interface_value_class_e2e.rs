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

#[test]
fn same_name_same_arity_sam_keeps_the_abstract_value_class_slot() {
    // `take(Left)` and `take(Right)` share an owner, a name, and an arity. Their value-class
    // hashes differ. The lambda implements the abstract `take(Left)` slot; binding the concrete
    // sibling's hash makes the first call throw AbstractMethodError.
    const SRC: &str = "@JvmInline\n\
value class Left(val n: Int)\n\
@JvmInline\n\
value class Right(val n: Int)\n\
fun interface Pick {\n\
    fun take(value: Left): Int\n\
    fun take(value: Right): Int = value.n\n\
}\n\
fun feed(pick: Pick): Int = pick.take(Left(3))\n\
fun box(): String {\n\
    val seen = feed { it.n + 1 }\n\
    return if (seen == 4) \"OK\" else \"fail: $seen\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "SameNameSameArityValueClassSam");
}

/// `LambdaMetafactory` cannot adapt a lambda whose interface slot instantiates a value class over a
/// non-null reference, or whose implementation must box a value class into a slot the interface
/// declares as that value class. kotlinc writes such a conversion as a class: it declares the
/// interface's own type parameters, implements the interface raw, holds the body under the method's
/// instantiated (mangled) name, and bridges the declared slot to it.
const FACTORY_CONFLICTS: &str = "\
@JvmInline value class IC(val v: String)
@JvmInline value class Boxed(val v: Any)
fun interface G<A : CharSequence, B> { fun g(a: A): IC }
fun interface H { fun h(x: Int): Int }
fun interface K<T> { fun k(x: T): T }
fun interface Call<T, R> { fun call(ic: T): R }
fun <X> use(g: G<String, X>) = g.g(\"a\")
fun <T, R> bar(value: T, f: Call<T, R>): R = f.call(value)
fun box(): String {
    val g = use<Int> { IC(it) }
    val h = H { it + 1 }
    val k = K<IC> { it }
    val boxed = bar<Boxed, String>(Boxed(\"b\")) { it.v as String }
    return g.v + h.h(1) + k.k(IC(\"z\")).v + boxed
}
";

#[test]
fn factory_conflicting_conversions_are_kotlincs_classes() {
    for class in [
        "FactoryConflictsKt$box$g$1",
        "FactoryConflictsKt$box$k$1",
        "FactoryConflictsKt$box$boxed$1",
        "FactoryConflictsKt",
    ] {
        common::assert_class_matches_kotlinc("FactoryConflicts", FACTORY_CONFLICTS, class);
    }
}
