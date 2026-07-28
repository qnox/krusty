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
