//! An inline function parameter receives a callable reference as the call its adapter returns,
//! not as a function-reference object. The referenced declaration's kind is not what makes the
//! argument spliceable.

use super::common;

const SRC: &str = "\
class Holder(val name: String)\n\
\n\
fun makeHolder(name: String) = Holder(name)\n\
\n\
inline fun <T, R> applyEach(value: T, transform: (T) -> R): R = transform(value)\n\
\n\
fun unbound(name: String) = applyEach(name, ::makeHolder)\n\
\n\
class Box(val prefix: String) {\n\
    fun label(name: String) = prefix + name\n\
}\n\
\n\
fun bound(box: Box, name: String) = applyEach(name, box::label)\n\
\n\
fun local(prefix: String, name: String): String {\n\
    fun label(value: String) = prefix + value\n\
    return applyEach(name, ::label)\n\
}\n\
\n\
fun names(values: List<String>): List<Holder> = values.map(::Holder)\n\
\n\
fun mapped(values: List<String>): List<Holder> = values.map(::makeHolder)\n\
";

fn method_matches(method: &str) {
    let difference = common::class_bytes_diff_against_kotlinc(
        "InlineCtorRef",
        &[],
        SRC,
        "InlineCtorRefKt",
        method,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(difference, Ok(()));
}

#[test]
fn an_inline_map_of_a_constructor_reference_matches_kotlinc() {
    method_matches("public static final java.util.List<Holder> names(");
}

#[test]
fn an_inline_map_of_a_function_reference_matches_kotlinc() {
    method_matches("public static final java.util.List<Holder> mapped(");
}

#[test]
fn an_unbound_function_reference_in_an_inline_call_matches_kotlinc() {
    method_matches("public static final Holder unbound(");
}

#[test]
fn a_bound_reference_in_an_inline_call_matches_kotlinc() {
    method_matches("public static final java.lang.String bound(");
}

#[test]
fn a_local_function_reference_in_an_inline_call_matches_kotlinc() {
    method_matches("public static final java.lang.String local(");
}

#[test]
fn a_bound_reference_with_a_default_argument_stays_callable() {
    common::expect_box_ok_with_stdlib(
        "\
class C {\n\
    fun ffff(i: Int, s: String = \"OK\") = s\n\
}\n\
fun box(): String = 42.run(C()::ffff)\n\
",
        "BoundAdaptedRef",
    );
}

#[test]
fn a_reified_extension_reference_in_let_runs() {
    common::expect_box_ok_with_stdlib(
        "\
object Foo {\n\
    val log = \"123\"\n\
}\n\
public inline fun <reified T> Foo.foo(value: T): String = log + value\n\
fun box(): String {\n\
    val result = \"OK\".let(Foo::foo)\n\
    if (result != \"123OK\") return result\n\
    return \"OK\"\n\
}\n\
",
        "ReifiedExtensionRef",
    );
}

#[test]
fn a_safe_call_let_of_a_callable_reference_returns_the_value() {
    common::expect_box_ok_with_stdlib(
        "\
fun <T> id(x: T): T = x\n\
fun <T> String.extId(x: T): T = x\n\
fun <T> foo(value: T?): T? = value?.let(::id)\n\
fun <T> bar(value: T?): T? = value?.let(\"\"::extId)\n\
fun box() = foo(\"O\")!! + bar(\"K\")!!\n\
",
        "SafeCallLetRef",
    );
}

#[test]
fn a_unit_bound_reference_in_for_each_still_runs() {
    common::expect_box_ok_with_stdlib(
        "\
class Foo\n\
\n\
class Builder {\n\
    var size: Int = 0\n\
\n\
    fun addFoo(foo: Foo): Builder {\n\
        size++\n\
        return this\n\
    }\n\
}\n\
\n\
fun box(): String {\n\
    val b = Builder()\n\
    listOf(Foo(), Foo(), Foo()).forEach(b::addFoo)\n\
    return if (b.size == 3) \"OK\" else \"Fail\"\n\
}\n\
",
        "UnitBoundRef",
    );
}

#[test]
fn a_reference_whose_receiver_contains_a_reference_keeps_call_order() {
    common::expect_box_ok_with_stdlib(
        "\
class Foo(val a: String) {\n\
    fun test() = a\n\
}\n\
\n\
var effects = \"\"\n\
\n\
fun create(a: String): Foo {\n\
    effects += a\n\
    return Foo(a)\n\
}\n\
\n\
fun create2(a: String, f: () -> String): Foo {\n\
    effects += a\n\
    return Foo(a)\n\
}\n\
\n\
inline fun test(a: String, b: () -> String, c: () -> String, d: () -> String, e: String): String {\n\
    return a + b() + c() + d() + e\n\
}\n\
\n\
fun box(): String {\n\
    val result = test(create(\"A\").a, create(\"B\")::a, create(\"C\")::test, create2(\"E\", create(\"D\")::test)::test, create(\"F\").a)\n\
    if (effects != \"ABCDEF\") return \"fail 1: $effects\"\n\
    return if (result == \"ABCEF\") \"OK\" else \"fail 2: $result\"\n\
}\n\
",
        "NestedReceiverRef",
    );
}

#[test]
fn an_object_that_captures_an_inlined_reference_still_runs() {
    common::expect_box_ok_with_stdlib(
        "\
inline fun inlineFun(crossinline lambda: () -> String) =\n\
    object {\n\
        fun foo(): String = lambda.invoke()\n\
    }.foo()\n\
\n\
class A {\n\
    val prop = inlineFun(::bar)\n\
    fun bar() = \"OK\"\n\
}\n\
\n\
fun box(): String = A().prop\n\
",
        "InlineRefAnonObject",
    );
}

#[test]
fn argument_ordering_keeps_an_inline_lambdas_mutable_capture_cell() {
    common::expect_box_ok_with_stdlib(
        "\
inline fun invokeNow(action: () -> Unit) = action()\n\
\n\
fun box(): String {\n\
    var state = 0\n\
    val observe = { state }\n\
    invokeNow { state += 2 }\n\
    val result = observe()\n\
    return if (result == 2) \"OK\" else \"FAIL: $result\"\n\
}\n\
",
        "InlineMutableCaptureOrder",
    );
}

#[test]
fn a_stored_reference_stays_a_carrier_beside_an_inlined_use() {
    common::expect_box_ok_with_stdlib(
        "\
fun id(value: String) = value\n\
fun box(): String {\n\
    val kept: (String) -> String = ::id\n\
    return \"O\".let(::id) + kept(\"K\")\n\
}\n\
",
        "StoredAndInlinedRef",
    );
}
