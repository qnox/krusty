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
