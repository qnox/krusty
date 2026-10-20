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

/// Each consumer below takes the value at an `Object` slot, which kotlinc fills with the bare load.
fn assert_family_matches_kotlinc(name: &str, source: &str) {
    let source = format!("interface Marker\n{source}");
    common::byte_diff_against_kotlinc_cp(
        name,
        &source,
        &format!("{name}Kt"),
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{name}Kt differs from kotlinc: {diff}"));
}

const EXTERNAL_MEMBER: &str = "fun toList(value: Any, list: MutableList<Any>): Boolean =\n\
    \x20   if (value is Marker) list.add(value) else false\n\
    fun toArrayList(value: Any, list: ArrayList<Any>): Boolean =\n\
    \x20   if (value is Marker) list.add(value) else false\n\
    fun toBuilder(value: Any, builder: StringBuilder) {\n\
    \x20   if (value is Marker) builder.append(value)\n\
    }\n";

/// A member's `$default` stub takes its receiver as a parameter of the declaring class, so a
/// receiver smart cast to a subclass is materialized at that class only.
const DEFAULT_DISPATCH: &str = "fun withDefault(value: Any?, n: Int = 1): String = \"d$n\"\n\
    fun toDefault(value: Any): String = if (value is Marker) withDefault(value) else \"not\"\n\
    open class Base { fun member(value: Any?, n: Int = 1): String = \"m$n\" }\n\
    class Sub : Base(), Marker\n\
    fun toMemberDefault(value: Any): String = if (value is Sub) value.member(value) else \"not\"\n\
    fun toMemberArgument(base: Base, value: Any): String =\n\
    \x20   if (value is Marker) base.member(value) else \"not\"\n";

/// A generic constructor parameter is an erased `Object`, whatever `T` is inferred to.
const CONSTRUCTOR: &str = "class Holder<T>(val value: T)\n\
    class AnyHolder(val value: Any?)\n\
    fun toConstructor(value: Any): Holder<*>? = if (value is Marker) Holder(value) else null\n\
    fun toAnyConstructor(value: Any): AnyHolder? = if (value is Marker) AnyHolder(value) else null\n";

/// `FunctionN.invoke` takes every argument as `Object`, whatever the function type's parameter.
const INVOKE: &str = "fun toInvoke(value: Any, f: (Marker) -> String): String =\n\
    \x20   if (value is Marker) f(value) else \"not\"\n\
    fun toAnyInvoke(value: Any, f: (Any?) -> String): String =\n\
    \x20   if (value is Marker) f(value) else \"not\"\n";

const EQUALITY: &str = "fun toEquals(value: Any, other: Any?): Boolean =\n\
    \x20   if (value is Marker) value == other else false\n";

/// `x` is a literal the caller passes, invoked from inside the lambda `around` passes to another
/// inline function: an inlined function-value alias.
const FUNCTION_ALIAS: &str = "inline fun <R> apply1(f: () -> R): R = f()\n\
    inline fun around(v: Any, x: (Marker) -> String): String =\n\
    \x20   apply1 { if (v is Marker) x(v) else \"not\" }\n\
    fun toAlias(value: Any): String = around(value) { \"$it\" }\n\
    inline fun pass(value: Any?, f: (Any?) -> String): String = f(value)\n\
    fun toPassed(value: Any, g: (Any?) -> String): String =\n\
    \x20   if (value is Marker) pass(value, g) else \"not\"\n";

#[test]
fn an_external_member_call_takes_a_smart_cast_argument_uncast() {
    assert_family_matches_kotlinc("SmartCastExternalMember", EXTERNAL_MEMBER);
}

#[test]
fn a_default_dispatch_takes_a_smart_cast_argument_uncast() {
    assert_family_matches_kotlinc("SmartCastDefaultDispatch", DEFAULT_DISPATCH);
}

#[test]
fn a_constructor_takes_a_smart_cast_argument_uncast() {
    assert_family_matches_kotlinc("SmartCastConstructor", CONSTRUCTOR);
}

#[test]
fn a_function_value_invoke_takes_a_smart_cast_argument_uncast() {
    assert_family_matches_kotlinc("SmartCastInvoke", INVOKE);
}

#[test]
fn structural_equality_takes_a_smart_cast_operand_uncast() {
    assert_family_matches_kotlinc("SmartCastEquality", EQUALITY);
}

#[test]
fn an_inlined_function_value_takes_a_smart_cast_argument_uncast() {
    assert_family_matches_kotlinc("SmartCastFunctionAlias", FUNCTION_ALIAS);
}

#[test]
fn smart_cast_values_reach_every_object_slot_and_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "interface Marker\n\
             class Leaf : Marker {{ override fun toString() = \"Leaf\" }}\n\
             {EXTERNAL_MEMBER}{DEFAULT_DISPATCH}{CONSTRUCTOR}{INVOKE}{EQUALITY}{FUNCTION_ALIAS}\
             fun box(): String {{\n\
             \x20   val leaf = Leaf()\n\
             \x20   val list = mutableListOf<Any>()\n\
             \x20   if (!toList(leaf, list) || list.single() !== leaf) return \"list\"\n\
             \x20   if (!toArrayList(leaf, ArrayList())) return \"array list\"\n\
             \x20   val builder = StringBuilder()\n\
             \x20   toBuilder(leaf, builder)\n\
             \x20   if (builder.toString() != \"Leaf\") return \"builder\"\n\
             \x20   if (toDefault(leaf) != \"d1\" || toDefault(1) != \"not\") return \"default\"\n\
             \x20   if (toMemberDefault(Sub()) != \"m1\" || toMemberArgument(Base(), leaf) != \"m1\") return \"member\"\n\
             \x20   if (toConstructor(leaf)?.value !== leaf || toAnyConstructor(leaf)?.value !== leaf) return \"constructor\"\n\
             \x20   if (toInvoke(leaf) {{ \"f$it\" }} != \"fLeaf\" || toAnyInvoke(leaf) {{ \"a$it\" }} != \"aLeaf\") return \"invoke\"\n\
             \x20   if (!toEquals(leaf, leaf) || toEquals(leaf, 1)) return \"equality\"\n\
             \x20   if (toAlias(leaf) != \"Leaf\" || toPassed(leaf) {{ \"g$it\" }} != \"gLeaf\") return \"alias\"\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "SmartCastConsumerFamilies",
    );
}
