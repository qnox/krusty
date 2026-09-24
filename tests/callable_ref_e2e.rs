//! Unbound top-level function references `::foo` passed to a function-typed parameter. Lowered to the
//! same `invokedynamic` + `LambdaMetafactory` machinery as a lambda, with the impl method handle
//! pointing directly at the referenced function. Round-tripped against the JVM under `-Xverify:all`.

use super::common;

#[test]
fn callable_refs_run() {
    const SRC: &str = "fun inc(n: Int): Int = n + 1\n\
fun twice(n: Int): Int = n * 2\n\
fun apply1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun box(): String {\n\
if (apply1(::inc, 41) != 42) return \"f1\"\n\
if (apply1(::twice, 21) != 42) return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "C");
}

#[test]
fn array_factory_references_use_the_selected_compiler_declaration() {
    const SRC: &str = r#"
fun collect(factory: (Int, Int) -> Array<Int>): String {
    val values = factory('O'.toInt(), 'K'.toInt())
    return "${values[0].toChar()}${values[1].toChar()}"
}

fun pass(factory: (Array<String>) -> Array<String>): Array<String> =
    factory(arrayOf("OK"))

fun box(): String {
    if (collect(::arrayOf) != "OK") return "adapted"
    val original = arrayOf("OK")
    val identity: (Array<String>) -> Array<String> = ::arrayOf
    if (identity(original) !== original) return "vararg identity"
    if (pass(::arrayOf)[0] != "OK") return "array parameter"
    val nullable: (Int) -> Array<String?> = ::arrayOfNulls
    val nulls = nullable(2)
    if (nulls.size != 2 || nulls[0] != null || nulls[1] != null) return "null factory"
    return "OK"
}
"#;
    common::expect_box_ok_with_stdlib(SRC, "ArrayFactoryCallableRef");
}

#[test]
fn reflective_string_plus_reference_invokes_provider_selected_intrinsic_adapter() {
    common::expect_box_ok_with_stdlib(
        r#"
fun box(): String {
    if ((String::plus)("O", "K") != "OK") return "plus"
    return "OK"
}
"#,
        "ReflectiveStringPlusIntrinsicReference",
    );
}

/// A reference to a builtin scalar member (`Int::times`) names a declaration with no JVM method.
/// The adapter handed to an inline stdlib function (`reduce`), an ordinary function value, a bound
/// receiver, and a reflective `KFunction` must each realize the declaration's primitive operation,
/// including numeric promotion (`Int.plus(Long)`), `Char` arithmetic, shifts, bit and unary operations,
/// conversions, `compareTo`, and a platform-typed (`Int!`) Java functional parameter.
#[test]
fn builtin_scalar_member_references_realize_their_primitive_operations() {
    const SRC: &str = r#"
import kotlin.reflect.KFunction1
import kotlin.reflect.KFunction2

fun product(w: List<Int>): Int = w.reduce(Int::times)
fun mixed(f: (Int, Long) -> Long) = f(2, 40L)
fun chars(f: (Char, Char) -> Int) = f('d', 'a')
fun charOffset(f: (Char, Int) -> Char) = f('a', 2)
fun bytes(f: (Byte, Byte) -> Int) = f(3, 4)
fun compare(f: (Int, Double) -> Int) = f(3, 2.5)

fun box(): String {
    if (product(listOf(2, 3, 4)) != 24) return "inline reduce"
    val times: (Int, Int) -> Int = Int::times
    if (times(6, 7) != 42) return "function value"
    if (mixed(Int::plus) != 42L) return "promoted plus"
    if (chars(Char::minus) != 3) return "char minus"
    if (charOffset(Char::plus) != 'c') return "char plus"
    if (bytes(Byte::times) != 12) return "byte times"
    if (listOf(7.0, 2.0).reduce(Double::div) != 3.5) return "double div"
    if (listOf(7L, 2L).fold(10L, Long::rem) != 1L) return "long rem"
    if (listOf(true, false).reduce(Boolean::xor) != true) return "boolean xor"
    if (listOf(6, 3).reduce(Int::and) != 2) return "and"
    if (listOf(3, 2).map(1L::shl) != listOf(8L, 4L)) return "bound shl"
    if (listOf(3, -2).map(Int::unaryMinus) != listOf(-3, 2)) return "unary minus"
    if (listOf(1, 2).map(Int::inv) != listOf(-2, -3)) return "inv"
    if (listOf(3.7, -2.2).map(Double::toInt) != listOf(3, -2)) return "conversion"
    if (listOf(true, false).map(Boolean::not) != listOf(false, true)) return "not"
    if (compare(Int::compareTo) != 1) return "compare"
    val counts = java.util.HashMap<String, Int>()
    counts["a"] = 1
    counts.merge("a", 2, Int::plus)
    if (counts["a"] != 3) return "platform merge"
    val bound: KFunction1<Int, Int> = 5::times
    if (bound(6) != 30 || bound.name != "times") return "reflective bound"
    val shift: KFunction2<Long, Int, Long> = Long::ushr
    if (shift(-1L, 60) != 15L) return "reflective ushr"
    return "OK"
}
"#;
    common::expect_box_ok_with_stdlib(SRC, "BuiltinScalarMemberReferences");
}

#[test]
fn inline_extension_plan_names_its_receiver_semantically() {
    const SRC: &str = r#"
interface T {
    fun foo() = "OK"
}

class B : T {
    inner class C {
        fun bar() = (T::foo).let { it(this@B) }
    }
}

fun box(): String = B().C().bar()
"#;
    common::expect_box_ok_with_stdlib(SRC, "InlineExtensionReceiverPlan");
}

#[test]
fn callable_reference_above_numbered_jvm_arity_uses_function_n() {
    const SRC: &str = r#"
var result = "FAIL"

fun target(
    a01: Int, a02: Int, a03: Int, a04: Int, a05: Int, a06: Int, a07: Int, a08: Int,
    a09: Int, a10: Int, a11: Int, a12: Int, a13: Int, a14: Int, a15: Int, a16: Int,
    a17: Int, a18: Int, a19: Int, a20: Int, a21: Int, a22: Int, a23: Int, a24: Int
) {
    if (a01 == 1 && a12 == 12 && a22 == 22 && a24 == 24) result = "OK"
}

fun box(): String {
    val reference = ::target
    reference(
        1, 2, 3, 4, 5, 6, 7, 8,
        9, 10, 11, 12, 13, 14, 15, 16,
        17, 18, 19, 20, 21, 22, 23, 24
    )
    return result
}
"#;
    common::expect_box_ok_with_stdlib(SRC, "HighArityCallableRef");
}

#[test]
fn inner_constructor_reference_preserves_outer_and_inner_type_arguments() {
    const SRC: &str = r#"
var result = "FAIL"

class Outer<T> {
    inner class Inner<U> {
        constructor() { result = "OK" }
    }
}

fun box(): String {
    val construct: Outer<*>.() -> Outer<out Any?>.Inner<out String> =
        Outer<out Any?>::Inner
    Outer<Int>().construct()
    return result
}
"#;
    common::expect_box_ok_with_stdlib(SRC, "GenericInnerConstructorRef");
}

#[test]
fn expected_type_selects_an_overloaded_toplevel_reference_and_coerces_unit() {
    const SRC: &str = "var result = \"FAIL\"\n\
fun choose(x: Int, y: Any): Int { result = \"OK\"; return x }\n\
fun choose(x: Any, y: Int): Int { result = \"wrong\"; return y }\n\
fun box(): String {\n\
    val selected: (Int, Any) -> Unit = ::choose\n\
    selected(1, \"\")\n\
    return result\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "OverloadedTopLevelRef");
}

#[test]
fn expected_type_uses_function_parameter_contravariance_and_return_covariance() {
    const SRC: &str = "fun convert(value: Any): String = value as String\n\
fun convert(value: Int): Int = value\n\
fun box(): String {\n\
    val selected: (String) -> Any = ::convert\n\
    return selected(\"OK\") as String\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "VariantTopLevelRef");
}

#[test]
fn expected_type_boxes_a_primitive_for_a_reference_supertype_parameter() {
    const SRC: &str =
        "fun convert(value: Number): String = if (value.toInt() == 1) \"OK\" else \"FAIL\"\n\
fun box(): String {\n\
    val selected: (Int) -> String = ::convert\n\
    return selected(1)\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "BoxedVariantTopLevelRef");
}

#[test]
fn expected_type_selects_the_most_specific_compatible_parameter_overload() {
    const SRC: &str = "fun convert(value: Any): Any = \"wrong\"\n\
fun convert(value: CharSequence): Any = value\n\
fun box(): String {\n\
    val selected: (String) -> Any = ::convert\n\
    return selected(\"OK\") as String\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "SpecificTopLevelRef");
}

#[test]
fn return_exactness_does_not_hide_a_more_specific_parameter_overload() {
    const SRC: &str = "fun pick(value: Any): Any = \"wrong\"\n\
fun pick(value: CharSequence): String = value.toString()\n\
fun box(): String {\n\
    val selected: (String) -> Any = ::pick\n\
    return selected(\"OK\") as String\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "ReturnBiasedTopLevelRef");
}

#[test]
fn bound_member_ref_flows_to_classpath_map() {
    const SRC: &str = "class C(val base: Int) {\n\
fun inc(x: Int) = x + 1\n\
fun add(a: Int, b: Int) = a + b + base\n\
}\n\
fun box(): String {\n\
val c = C(10)\n\
if (c.inc(5) != 6) return \"f1\"\n\
if (c.add(2, 3) != 15) return \"f2\"\n\
val r = listOf(1, 2, 3).map(c::inc)\n\
if (r != listOf(2, 3, 4)) return \"f3:$r\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "BoundMapRef");
}

#[test]
fn zero_arg_lambda_targeting_maps_member_arguments() {
    const SRC: &str = "class Item\n\
class Picker {\n\
    fun select(candidate: Item?): Item? {\n\
        val value = candidate?.let {\n\
            guarded { convert(it) }\n\
        }\n\
        return value\n\
    }\n\
    fun direct(): Item? = this.guarded { Item() }\n\
    fun safe(other: Picker?): Item? = other?.guarded { Item() }\n\
    fun named(): Item? = guarded(body = { Item() })\n\
    fun directNamed(): Item? = this.guarded(body = { Item() })\n\
    fun namedTrailing(): Item? = guarded(tag = 1) { Item() }\n\
    fun directNamedTrailing(): Item? = this.guarded(tag = 1) { Item() }\n\
    private fun convert(value: Item): Item? = value\n\
    private fun <T> guarded(tag: Int = 0, body: () -> T): T? = body()\n\
}\n";
    let diags = common::checker_diags_with_stdlib(SRC)
        .expect("stdlib is required for generic member diagnostics");
    assert!(diags.is_empty(), "{diags:#?}");
}

#[test]
fn zero_arg_lambda_to_generic_member_runs() {
    const SRC: &str = "class Item\n\
class Picker {\n\
    fun select(candidate: Item?): Item? = candidate?.let { guarded { convert(it) } }\n\
    fun direct(): Item? = this.guarded { Item() }\n\
    fun safe(other: Picker?): Item? = other?.guarded { Item() }\n\
    private fun convert(value: Item): Item? = value\n\
    private fun <T> guarded(body: () -> T): T? = body()\n\
}\n\
fun box(): String {\n\
    val picker = Picker()\n\
    return if (picker.select(Item()) != null && picker.direct() != null && picker.safe(picker) != null) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "ZeroArgGenericMember");
}

#[test]
fn nested_lambda_keeps_classpath_receiver_type_for_overload() {
    const LIBRARY: &str = r#"
package fixture

open class Item
class Scope
class Module
"#;
    const MAIN: &str = r#"
import fixture.Item
import fixture.Module
import fixture.Scope

class Picker {
    private fun resolve(item: Item, scope: Scope?): Item? = item
    private fun resolve(item: Item, module: Module): Item? = item
    private fun <T> guarded(body: () -> T): T? = body()

    fun select(original: Item?, current: Item, module: Module): Item? {
        val value = original?.takeUnless(current::equals)?.let {
            guarded { resolve(it, module) }
        }
        return value
    }
}

fun box(): String {
    val original = Item()
    val selected = Picker().select(original, Item(), Module())
    return if (selected === original) "OK" else "fail"
}
"#;

    common::expect_box_ok_against("nested_lambda_classpath_receiver", LIBRARY, MAIN);
}

#[test]
fn property_ref_keeps_api_and_fits_function_shape() {
    const SRC: &str = "class C(val n: Int)\n\
fun apply1(f: (C) -> Int, c: C): Int = f(c)\n\
fun box(): String {\n\
val p = C::n\n\
if (p.get(C(3)) != 3) return \"get\"\n\
if (p.name != \"n\") return \"name:${p.name}\"\n\
val f: (C) -> Int = p\n\
if (f(C(4)) != 4) return \"fun\"\n\
if (apply1(p, C(5)) != 5) return \"hof\"\n\
if (listOf(C(6)).map(p)[0] != 6) return \"map\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "PropertyRefShape");
}

#[test]
fn class_literal_type_is_provider_backed() {
    // `C::class` is a `kotlin.reflect.KClass` (emitted via `Reflection.getOrCreateKotlinClass`), so it
    // exposes the KClass API (`simpleName`) — NOT `java.lang.Class`'s `name`, which is only reachable
    // through the `.java` bridge. (This asserted `c.name` while krusty modelled a class literal as a bare
    // `java.lang.Class`; that shape does not compile under kotlinc.)
    const SRC: &str = "class C\n\
fun box(): String {\n\
val c = C::class\n\
val n = c.simpleName ?: return \"null\"\n\
if (n != \"C\") return n\n\
return if (c.java.name.endsWith(\"C\")) \"OK\" else c.java.name\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "ClassLiteralShape");
}

/// A callable reference / class literal on a NULLABLE receiver type (`A?::foo`, `A?::class`). The `?`
/// only marks the receiver type nullable; the reference is the same callable. Previously the parser
/// emitted "expected an expression" at `?::`.
#[test]
fn nullable_receiver_callable_ref_runs() {
    const SRC: &str = "class A { fun foo(): String = \"OK\" }\n\
fun box(): String {\n\
    val r: (A) -> String = A?::foo\n\
    val a = A()\n\
    if (r(a) != \"OK\") return \"f1\"\n\
    if (A?::class.simpleName != \"A\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "CR");
}

/// An unbound member reference on a GENERIC class with explicit type arguments (`A<String>::foo`).
/// The type arguments erase, so it references `A::foo`. Previously the parser misread `A<String>` as a
/// less-than comparison chain and reported `unresolved reference 'A'`.
#[test]
fn generic_class_unbound_member_ref_runs() {
    const SRC: &str = "class A<T>(val t: T) { fun foo(): T = t }\n\
fun box(): String = (A<String>::foo).let { it(A(\"OK\")) }\n";
    common::expect_box_ok_with_stdlib(SRC, "CR");
}

/// ADAPTED bound member references: a reference to a member with a trailing `vararg` and/or a default
/// parameter, used where a lower-arity functional type is expected (`C(7)::mv` as `(Int) -> String`).
/// The lowerer's synthesized adapter fills the empty vararg / default via the member's `$default` stub.
#[test]
fn adapted_bound_member_ref_runs() {
    const SRC: &str = "// WITH_STDLIB\n\
class C(val e: Int) {\n\
    fun mv(i: Int, vararg s: String): String = if (i == e && s.isEmpty()) \"\" else \"bad\"\n\
    fun md(i: Int, s: String = \"d\"): String = if (i == e && s == \"d\") \"\" else \"bad\"\n\
    fun mb(i: Int, s: String = \"d\", vararg t: String): String = if (i == e && s == \"d\" && t.isEmpty()) \"\" else \"bad\"\n\
}\n\
fun test(f: (Int) -> String, p: Int): String = f(p)\n\
fun box(): String {\n\
    if (test(C(7)::mv, 7) != \"\") return \"f1\"\n\
    if (test(C(7)::md, 7) != \"\") return \"f2\"\n\
    if (test(C(7)::mb, 7) != \"\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "CR");
}
