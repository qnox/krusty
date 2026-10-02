//! A class/object may declare a FUNCTION TYPE as a supertype (`class C : () -> R`), implementing the
//! JVM functional interface `kotlin/jvm/functions/FunctionN`. The class provides `override fun invoke`,
//! and an instance is assignable to the matching `(…) -> R` and callable as a function value. Covers
//! nullary, parameterised, `object`, and extension-receiver (`Recv.() -> R`) forms. Runs on the JVM.
//!
//! The other direction: a function type is a `kotlin.Function<out R>` of its OWN result `R` —
//! `() -> String` is a `Function<CharSequence>`, never a `Function<Int>`. The hierarchy walk reaches
//! `Function` from a function type; the type-argument check must then run on what it reached.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nullary_function_supertype_called_as_value() {
    const SRC: &str = "class C : () -> String {\n\
        \x20 override fun invoke(): String = \"OK\"\n\
        }\n\
        fun box(): String {\n\
        \x20 val f: () -> String = C()\n\
        \x20 return f()\n\
        }\n";
    assert_eq!(run(SRC).expect("nullary fn supertype"), "OK");
}

#[test]
fn parameterised_function_supertype() {
    const SRC: &str = "class Add : (Int, Int) -> Int {\n\
        \x20 override fun invoke(a: Int, b: Int): Int = a + b\n\
        }\n\
        fun box(): String {\n\
        \x20 val f: (Int, Int) -> Int = Add()\n\
        \x20 return if (f(2, 3) == 5) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("param fn supertype"), "OK");
}

#[test]
fn object_function_supertype() {
    const SRC: &str = "object Greet : () -> String {\n\
        \x20 override fun invoke(): String = \"OK\"\n\
        }\n\
        fun box(): String {\n\
        \x20 val g: () -> String = Greet\n\
        \x20 return g()\n\
        }\n";
    assert_eq!(run(SRC).expect("object fn supertype"), "OK");
}

#[test]
fn function_type_as_type_argument_is_not_a_function_supertype() {
    // Regression: a generic supertype whose type ARGUMENT is a function type (`Base<() -> String>`)
    // must NOT be misread as a function-type supertype — the `->` sits inside `<…>`, at depth > 0.
    const SRC: &str = "open class Base<T>\n\
        class C : Base<() -> String>() {\n\
        \x20 fun ok(): String = \"OK\"\n\
        }\n\
        fun box(): String = C().ok()\n";
    assert_eq!(run(SRC).expect("fn type as type arg"), "OK");
}

#[test]
fn extension_receiver_function_supertype() {
    // `Recv.() -> R` folds the receiver into the first `FunctionN` parameter (`Function1<Recv, R>`),
    // so the class implements `Function1` with an `invoke(Recv)` (calling it via receiver syntax
    // `5.h()` is a separate function-value-invoke path — here the interface method is called directly).
    const SRC: &str = "class Ext : Int.() -> Int {\n\
        \x20 override fun invoke(x: Int): Int = x + 1\n\
        }\n\
        fun box(): String {\n\
        \x20 val h = Ext()\n\
        \x20 return if (h.invoke(5) == 6) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("extension-receiver fn supertype"), "OK");
}

#[test]
fn contextual_extension_function_supertype_called_directly() {
    const SRC: &str = "// LANGUAGE: +ContextParameters, +FunctionalTypeWithExtensionAsSupertype\n\
        class Part(val text: String)\n\
        class Join : context(Part) Part.(Part) -> String {\n\
        \x20 override fun invoke(context: Part, receiver: Part, value: Part): String =\n\
        \x20     context.text + receiver.text + value.text\n\
        }\n\
        fun box(): String = Join()(Part(\"O\"), Part(\"K\"), Part(\"\"))\n";
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn a_function_type_is_a_function_of_a_supertype_of_its_result() {
    const SRC: &str = "fun takesStrings(f: Function<String>): String = \"s\"\n\
fun takesCharSequences(f: Function<CharSequence>): String = \"c\"\n\
fun box(): String {\n\
    val l: () -> String = { \"x\" }\n\
    val any: Function<Any> = l\n\
    val strings: Function<String> = l\n\
    return if (takesStrings(l) + takesCharSequences(l) == \"sc\" && any === strings) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "FunctionOfSupertype");
}

#[test]
fn a_function_type_is_not_a_function_of_an_unrelated_result() {
    let assignment = common::front_end_diagnostics_files_with_stdlib(&["fun f() {\n\
    val l: () -> String = { \"x\" }\n\
    val ints: Function<Int> = l\n\
}\n"]);
    assert_eq!(
        assignment,
        ["initializer type mismatch: expected 'Function<Int>', actual '() -> String'."],
        "complete assignment diagnostics"
    );
    let argument =
        common::front_end_diagnostics_files_with_stdlib(&["fun takesInts(f: Function<Int>) {}\n\
fun f() {\n\
    val l: () -> String = { \"x\" }\n\
    takesInts(l)\n\
}\n"]);
    assert_eq!(
        argument,
        ["argument type mismatch: actual type is '() -> String', but 'Function<Int>' was expected."],
        "complete argument diagnostics"
    );
}

/// A suspend function type as a supertype is realized as `Function{N+1}` with the continuation as
/// its last type argument, plus the `SuspendFunction` marker; kotlinc's `invoke(Object, Object)`
/// bridge casts that `Object` to `Continuation`, and `@Metadata` records the supertype as
/// `Function{N+1}<…, Continuation<R>, Any?>` flagged suspend.
#[test]
fn a_suspend_function_supertype_matches_kotlinc() {
    const SRC: &str = "class Probe : suspend (Int) -> String {\n\
    override suspend fun invoke(p1: Int): String = \"OK\"\n\
}\n";
    common::byte_diff_against_kotlinc("SuspendFunctionSupertype", SRC, "Probe")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

/// The erased `invoke(Object, Object)` a caller reaches through `Function2` delegates to the
/// suspend override with the object cast back to a `Continuation`.
#[test]
fn a_suspend_function_supertype_is_invoked_through_its_erased_bridge() {
    const SRC: &str = "import kotlin.coroutines.*\n\
class Probe : suspend (Int) -> String {\n\
    override suspend fun invoke(p1: Int): String = if (p1 == 7) \"OK\" else \"fail\"\n\
}\n\
object Done : Continuation<String> {\n\
    override val context: CoroutineContext get() = throw UnsupportedOperationException()\n\
    override fun resumeWith(result: Result<String>) {}\n\
}\n\
fun box(): String {\n\
    val erased: Any = Probe()\n\
    @Suppress(\"UNCHECKED_CAST\")\n\
    val physical = erased as (Int, Continuation<String>) -> Any?\n\
    return physical(7, Done) as String\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "SuspendFunctionSupertypeBridge");
}

/// Past the 22 numbered function interfaces a function type is `FunctionN`, whose single
/// `invoke(Object[])` kotlinc implements as a final bridge that checks the array length and casts
/// each element to the override's parameter.
#[test]
fn a_big_arity_function_supertype_matches_kotlinc() {
    const SRC: &str = "class Box(val value: String)\n\
class Wide : (Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box) -> String {\n\
    override fun invoke(p1: Box, p2: Box, p3: Box, p4: Box, p5: Box, p6: Box, p7: Box, p8: Box, p9: Box, p10: Box, p11: Box, p12: Box, p13: Box, p14: Box, p15: Box, p16: Box, p17: Box, p18: Box, p19: Box, p20: Box, p21: Box, p22: Box, p23: Box): String = p2.value + p21.value\n\
}\n";
    common::byte_diff_against_kotlinc("BigArityFunctionSupertype", SRC, "Wide")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

/// A call through the function type packs its arguments into the array the bridge unpacks.
#[test]
fn a_big_arity_function_supertype_is_invoked_through_its_array_bridge() {
    const SRC: &str = "class Box(val value: String)\n\
class Wide : (Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box) -> String {\n\
    override fun invoke(p1: Box, p2: Box, p3: Box, p4: Box, p5: Box, p6: Box, p7: Box, p8: Box, p9: Box, p10: Box, p11: Box, p12: Box, p13: Box, p14: Box, p15: Box, p16: Box, p17: Box, p18: Box, p19: Box, p20: Box, p21: Box, p22: Box, p23: Box): String = p2.value + p21.value\n\
}\n\
fun box(): String {\n\
    val b = Box(\"\")\n\
    val o = Box(\"O\")\n\
    val k = Box(\"K\")\n\
    val f: (Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box, Box) -> String = Wide()\n\
    return f(b, o, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, b, k, b, b)\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "BigArityFunctionSupertypeBridge");
}

/// A value class that implements a function type is still a carrier at a direct call. kotlinc calls
/// the static implementation (`invoke-impl` or the signature hash) with that carrier as argument
/// zero. Boxing it and calling `FunctionN.invoke` leaves an `int` where the interface expects an
/// object (`VerifyError`).
#[test]
fn a_value_class_extension_function_is_called_through_its_static_invoke() {
    const SRC: &str = "// LANGUAGE: +FunctionalTypeWithExtensionAsSupertype\n\
@JvmInline\n\
value class ValueClass(private val s: Int) : Int.() -> String {\n\
    override fun invoke(p1: Int): String = if (s == 1 && p1 == 1) \"OK\" else \"fail\"\n\
}\n\
fun box(): String = ValueClass(1)(1)\n";
    common::expect_box_same_as_kotlinc(SRC, "ValueClassExtensionFunction");
}

#[test]
fn a_value_class_function_call_keeps_the_carrier_in_every_direct_shape() {
    const SRC: &str = "// LANGUAGE: +FunctionalTypeWithExtensionAsSupertype\n\
@JvmInline\n\
value class Out(val v: Int)\n\
@JvmInline\n\
value class Id(val v: Int)\n\
@JvmInline\n\
value class ValueClass(private val s: Int) : Int.() -> String {\n\
    override fun invoke(p1: Int): String = \"${s}${p1}\"\n\
}\n\
@JvmInline\n\
value class Nullary(private val s: Int) : () -> String {\n\
    override fun invoke(): String = \"n$s\"\n\
}\n\
@JvmInline\n\
value class Name(val raw: String) : () -> String {\n\
    override fun invoke(): String = raw\n\
}\n\
@JvmInline\n\
value class TakesId(val s: Int) : (Id) -> String {\n\
    override fun invoke(p: Id): String = \"${s}${p.v}\"\n\
}\n\
@JvmInline\n\
value class MakesOut(val s: Int) : () -> Out {\n\
    override fun invoke(): Out = Out(s)\n\
}\n\
fun local(): String {\n\
    val v = ValueClass(1)\n\
    return v(2)\n\
}\n\
fun param(v: ValueClass): String = v(3)\n\
fun bang(v: ValueClass?): String = v!!(4)\n\
fun explicit(v: ValueClass): String = v.invoke(5)\n\
fun asFn(): String {\n\
    val f: Int.() -> String = ValueClass(6)\n\
    return f(7)\n\
}\n\
fun box(): String {\n\
    val seen = listOf(\n\
        ValueClass(1)(1), local(), param(ValueClass(1)), bang(ValueClass(1)),\n\
        explicit(ValueClass(1)), asFn(), Nullary(8)(), Name(\"N\")(),\n\
        TakesId(1)(Id(2)), MakesOut(9)().v.toString(),\n\
    ).joinToString(\",\")\n\
    return if (seen == \"11,12,13,14,15,67,n8,N,12,9\") \"OK\" else seen\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "ValueClassFunctionShapes");
}

#[test]
fn a_sibling_value_class_function_is_called_through_its_static_invoke() {
    const DECL: &str = "@JvmInline\n\
value class ValueClass(private val s: Int) : (Int) -> String {\n\
    override fun invoke(p1: Int): String = if (s == p1) \"OK\" else \"fail\"\n\
}\n";
    const CALL: &str = "fun box(): String = ValueClass(4)(4)\n";
    let sources = [("ValueClass.kt", DECL), ("Box.kt", CALL)];
    assert_eq!(common::kotlinc_box_files_result(&sources, "BoxKt"), "OK");
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
    assert_same_method(&sources, "BoxKt", "box");
}

/// Two `invoke` members of one arity lower to the same static name and a different descriptor.
/// Declaration order must not choose the target.
#[test]
fn same_arity_invoke_overloads_keep_the_selected_descriptor_in_either_order() {
    const INT_FIRST: &str = "@JvmInline\n\
value class IntFirst(val s: Int) : (String) -> String {\n\
    operator fun invoke(n: Int): String = \"i$n\"\n\
    override fun invoke(text: String): String = \"s$text\"\n\
}\n\
fun intFirst(): String = IntFirst(1)(2) + IntFirst(1)(\"a\")\n";
    const STRING_FIRST: &str = "@JvmInline\n\
value class StringFirst(val s: Int) : (Int) -> String {\n\
    override fun invoke(n: Int): String = \"i$n\"\n\
    operator fun invoke(text: String): String = \"s$text\"\n\
}\n\
fun stringFirst(): String = StringFirst(1)(2) + StringFirst(1)(\"a\")\n";
    let src = format!("{INT_FIRST}\n{STRING_FIRST}\nfun box(): String {{\n    val seen = intFirst() + stringFirst()\n    return if (seen == \"i2sa\" + \"i2sa\") \"OK\" else seen\n}}\n");
    common::expect_box_same_as_kotlinc(&src, "ValueClassInvokeOverloads");
    assert_same_method(
        &[("ValueClassInvokeOverloads.kt", src.as_str())],
        "ValueClassInvokeOverloadsKt",
        "intFirst",
    );
    assert_same_method(
        &[("ValueClassInvokeOverloads.kt", src.as_str())],
        "ValueClassInvokeOverloadsKt",
        "stringFirst",
    );
}

#[test]
fn a_generic_value_class_invoke_uses_the_substituted_member() {
    const SRC: &str = "@JvmInline\n\
value class Box<T : Any>(val value: T) : (T) -> String {\n\
    override fun invoke(item: T): String = value.toString() + item.toString()\n\
}\n\
fun box(): String = if (Box(\"O\")(\"K\") == \"OK\") \"OK\" else Box(\"O\")(\"K\")\n";
    common::expect_box_same_as_kotlinc(SRC, "GenericValueClassInvoke");
    assert_same_method(
        &[("GenericValueClassInvoke.kt", SRC)],
        "GenericValueClassInvokeKt",
        "box",
    );
}

#[test]
fn direct_narrowed_and_function_typed_value_class_calls_match_kotlinc() {
    const SRC: &str = "@JvmInline\n\
value class ValueClass(val s: Int) : (Int) -> String {\n\
    override fun invoke(p: Int): String = \"${s}${p}\"\n\
}\n\
fun direct(): String = ValueClass(1)(2)\n\
fun narrowed(v: ValueClass?): String = v!!(3)\n\
fun typed(): String {\n\
    val f: (Int) -> String = ValueClass(4)\n\
    return f(5)\n\
}\n\
fun box(): String {\n\
    val seen = direct() + narrowed(ValueClass(1)) + typed()\n\
    return if (seen == \"121345\") \"OK\" else seen\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "ValueClassInvokeAbi");
    let sources = [("ValueClassInvokeAbi.kt", SRC)];
    assert_same_method(&sources, "ValueClassInvokeAbiKt", "direct");
    assert_same_method(&sources, "ValueClassInvokeAbiKt", "narrowed-d8IPsTA");
    assert_same_method(&sources, "ValueClassInvokeAbiKt", "typed");
}

#[test]
fn a_compiled_dependency_value_class_invoke_uses_the_static_member() {
    const LIB: &str = "package lib\n\
@JvmInline\n\
value class ValueClass(val s: Int) : (Int) -> String {\n\
    override fun invoke(p: Int): String = \"${s}${p}\"\n\
}\n";
    const CALLER: &str = "import lib.ValueClass\n\
fun box(): String = if (ValueClass(1)(2) == \"12\") \"OK\" else ValueClass(1)(2)\n";
    let lib =
        common::kotlinc_lib_out(&[("Lib.kt", LIB)]).expect("reference kotlinc is provisioned");
    let pair = common::ModuleClassPair::compile_with_classpath(
        &[("DependencyValueClassInvoke.kt", CALLER)],
        &[lib],
        "DependencyValueClassInvokeKt",
    );
    let (kotlinc, krusty) = pair.method_code("DependencyValueClassInvokeKt", "box");
    assert_eq!(
        krusty, kotlinc,
        "DependencyValueClassInvokeKt.box instructions differ\n--- kotlinc ---\n{kotlinc}--- krusty ---\n{krusty}"
    );
}

fn assert_same_method(sources: &[(&str, &str)], class: &str, method: &str) {
    let pair = common::ModuleClassPair::compile(sources, class);
    let (kotlinc, krusty) = pair.method_code(class, method);
    assert_eq!(
        krusty, kotlinc,
        "{class}.{method} instructions differ\n--- kotlinc ---\n{kotlinc}--- krusty ---\n{krusty}"
    );
}
