//! Focused classpath resolver regressions. These duplicate a few cases from the larger feature bundle so
//! resolver/provider cleanup gets a small, direct failure when metadata or inline-overload selection drifts.

use super::common;

/// Strict stdlib/JDK run: missing tooling or a rejected source panics with diagnostics, so callers
/// cannot turn either failure into a passing skip.
fn run(src: &str, stem: &str) -> String {
    common::expect_box_run_with_stdlib(src, stem)
}

#[test]
fn inferred_function_call_types_an_inferred_property() {
    let src = r#"
fun answer() = 42
val answerValue = answer()
val widened = answerValue.toLong()
fun box(): String = if (widened == 42L) "OK" else "wrong"
"#;
    assert_eq!(run(src, "ResolverInferredFunctionProperty"), "OK");
}

#[test]
fn chained_generic_extension_call_types_an_inferred_property() {
    let src = r#"
val values = listOf("a", "b").asSequence()
fun use() = values.withIndex()
"#;
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        Vec::<String>::new()
    );
}

#[test]
fn extension_on_a_classpath_supertype_applies_to_its_implementation() {
    let src = r#"
fun indexed(builder: StringBuilder) = builder.withIndex()
"#;
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        Vec::<String>::new()
    );
}

#[test]
fn context_typed_lambda_selects_the_callable_for_property_inference() {
    let src = r#"
interface Consumer { fun consume(value: String) }
inline fun makeConsumer(crossinline block: (String) -> Unit) = object : Consumer {
    override fun consume(value: String) = block(value)
}
class Holder {
    var value = "wrong"
    val consumer = makeConsumer { value = it }
}
fun use() {
    val holder = Holder()
    holder.consumer.consume("OK")
}
"#;
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        Vec::<String>::new()
    );
}

#[test]
fn inherited_generic_extension_property_keeps_its_own_type_parameter_scope() {
    let src = r#"
open class Base {
    open val <T> (Int.() -> T).value: T get() = this(1)
}
class Derived : Base() {
    fun read(): String = (fun Int.(): String = "OK").value
}
fun use(): String = Derived().read()
"#;
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        Vec::<String>::new()
    );
}

#[test]
fn overloaded_metadata_return_does_not_pollute_progression_step() {
    let src = r#"
fun box(): String {
    val p = 1..10
    var s = 0
    for (i in p step 2) s += i
    if (s != 25) return "s=$s"
    var r = 0
    for (i in (1..9).reversed() step 2) r += i
    if (r != 25) return "r=$r"
    var t = 0
    for (i in p step 2 step 3) t += i
    return if (t == 12) "OK" else "t=$t"
}
"#;
    let out = run(src, "ResolverProgressionStep");
    assert_eq!(out, "OK");
}

#[test]
fn progression_step_with_a_top_level_minimum_bound_keeps_its_callable() {
    let src = r#"
val minimum = Int.MIN_VALUE
val longMinimum = Long.MIN_VALUE

fun box(): String {
    val values = ArrayList<Int>()
    val progression = (minimum + 5) downTo minimum step 3
    for (value in progression) values.add(value)
    if (values != listOf(minimum + 5, minimum + 2)) return values.toString()

    val longValues = ArrayList<Long>()
    val longProgression = (longMinimum + 5) downTo longMinimum step 3
    for (value in longProgression) longValues.add(value)
    return if (longValues == listOf(longMinimum + 5, longMinimum + 2)) "OK" else longValues.toString()
}
"#;
    assert_eq!(run(src, "ResolverBoundedProgressionStep"), "OK");
}

#[test]
fn callable_reference_keeps_its_signature_through_generic_let() {
    let src = r#"
class Result(val value: String)

fun box(): String = (::Result).let { it("OK") }.value
"#;
    assert_eq!(run(src, "ResolverCallableReferenceThroughLet"), "OK");
}

#[test]
fn callable_reference_return_inference_also_constrains_parameters() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
fun <T> consume(block: (T) -> T) {}
fun mismatched(value: String): Int = value.length
fun test() { consume(::mismatched) }
"#,
    );
    assert_eq!(
        diagnostics,
        vec!["none of the following candidates is applicable:"]
    );
}

#[test]
fn selected_empty_array_uses_the_target_element_type() {
    let src = r#"
fun accept(values: Array<String>): Array<String> = values

fun box(): String {
    val values = accept(emptyArray())
    return if (values.isEmpty()) "OK" else "not empty"
}
"#;
    assert_eq!(run(src, "ResolverSelectedEmptyArray"), "OK");
}

#[test]
fn source_empty_array_shadows_the_compiler_declaration() {
    let src = r#"
fun <T> emptyArray(): String = "user"
fun box(): String = emptyArray<Int>()
"#;
    assert_eq!(run(src, "ResolverShadowedEmptyArray"), "user");
}

#[test]
fn source_println_shadows_the_default_import() {
    let src = r#"
fun println(value: String): String = "user:$value"
fun box(): String = println("OK")
"#;
    assert_eq!(run(src, "ResolverShadowedPrintln"), "user:OK");
}

#[test]
fn core_any_constructor_is_an_ordinary_candidate_without_a_classpath() {
    let diagnostics = common::front_end_diagnostics_with_stdlib("fun make(): Any = Any()");
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn core_builtin_properties_are_ordinary_declarations_without_a_classpath() {
    let diagnostics = common::front_end_diagnostics(
        r#"
fun measure(text: String, values: IntArray, ch: Char): Int =
    text.length + values.size + ch.code
"#,
        &[],
        None,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn inapplicable_nearer_property_does_not_hide_extension_property() {
    let diagnostics = common::front_end_diagnostics(
        r#"
val code: String = "not an extension"
fun measure(ch: Char): Int = ch.code
"#,
        &[],
        None,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn exact_type_parameter_rejects_a_wider_explicit_argument() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use() = exact<CharSequence>("x")
"#,
    );
    assert_eq!(
        diagnostics,
        ["argument type mismatch: actual type is 'String', but 'CharSequence' was expected."]
    );
}

#[test]
fn source_char_code_extension_shadows_the_core_declaration() {
    let src = r#"
val Char.code: Int get() = 99
fun box(): String = if ('A'.code == 99) "OK" else "wrong"
"#;
    assert_eq!(run(src, "ResolverShadowedCharCode"), "OK");
}

#[test]
fn source_start_coroutine_extension_is_not_replaced_by_the_stdlib_intrinsic() {
    let src = r#"
fun (suspend () -> Unit).startCoroutine(value: String): String = value
suspend fun work() {}
fun box(): String = (::work).startCoroutine("OK")
"#;
    assert_eq!(run(src, "ResolverShadowedStartCoroutine"), "OK");
}

#[test]
fn coroutine_suspended_is_a_selected_property_without_a_classpath() {
    let diagnostics = common::front_end_diagnostics(
        r#"
import kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED
fun sentinel(): Any = COROUTINE_SUSPENDED
"#,
        &[],
        None,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn suspend_coroutine_intrinsic_uses_its_selected_stdlib_signature() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
import kotlin.coroutines.intrinsics.*
suspend fun suspendForever(): Int =
    suspendCoroutineUninterceptedOrReturn { COROUTINE_SUSPENDED }
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn suspend_coroutine_intrinsic_preserves_its_continuation_type() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
import kotlin.coroutines.resume
import kotlin.coroutines.intrinsics.*
suspend fun <T> await(value: T): T =
    suspendCoroutineUninterceptedOrReturn { continuation ->
        continuation.resume(value)
        COROUTINE_SUSPENDED
    }
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn nested_contextual_result_preserves_the_lambda_input_type_parameter() {
    let source = r#"
interface Marker { fun mark(): String }

fun <T> build(transform: (T) -> String): List<T> = TODO()
fun <U : Marker> outer(): List<U> = build { it.mark() }
"#;
    let Some((code, diagnostics)) = common::kotlinc_source_result("NestedContextResult", source)
    else {
        return;
    };
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(source),
        Vec::<String>::new()
    );
}

#[test]
fn repeated_contextual_result_constraints_do_not_choose_one_occurrence() {
    let source = r#"
interface Marker { fun mark(): String }
class Duo<A, B>

fun <T> build(transform: (T) -> String): Duo<T, T> = TODO()
fun <U : Marker> outer(): Duo<U, U?> = build { it.mark() }
"#;
    let Some((code, diagnostics)) = common::kotlinc_source_result("RepeatedContextResult", source)
    else {
        return;
    };
    assert_ne!(code, 0, "kotlinc accepted the conflicting fixture");
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(source),
        vec!["cannot infer type for type parameter 'T'. Specify it explicitly."]
    );
    assert!(
        diagnostics.contains("cannot infer type for type parameter 'T'")
            || diagnostics.contains("type mismatch"),
        "unexpected kotlinc diagnostic: {diagnostics}"
    );
}

#[test]
fn symbolic_and_concrete_contextual_result_constraints_conflict() {
    let source = r#"
interface Marker { fun mark(): String }
class Duo<A, B>

fun <T> build(transform: (T) -> String): Duo<T, T> = TODO()
fun <U : Marker> outer(): Duo<U, String> = build { it.mark() }
"#;
    let Some((code, diagnostics)) = common::kotlinc_source_result("MixedContextResult", source)
    else {
        return;
    };
    assert_ne!(code, 0, "kotlinc accepted the conflicting fixture");
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(source),
        vec!["cannot infer type for type parameter 'T'. Specify it explicitly."]
    );
    assert!(
        diagnostics.contains("cannot infer type for type parameter 'T'")
            || diagnostics.contains("type mismatch"),
        "unexpected kotlinc diagnostic: {diagnostics}"
    );
}

#[test]
fn nested_contextual_nullable_result_keeps_the_lambda_input_nullable() {
    let source = r#"
interface Marker { fun mark(): String }

fun <T> build(transform: (T) -> String): List<T> = TODO()
fun <T : Marker> outer(): List<T?> = build { it.mark() }
"#;
    let Some((code, diagnostics)) = common::kotlinc_source_result("NullableNestedContext", source)
    else {
        return;
    };
    assert_ne!(code, 0, "kotlinc accepted the invalid fixture");
    assert!(
        diagnostics.contains("only safe (?.) or non-null asserted (!!.) calls are allowed"),
        "unexpected kotlinc diagnostic: {diagnostics}"
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(source),
        vec!["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'T?'."]
    );
}

#[test]
fn unit_value_uses_the_selected_singleton_classifier() {
    let src = r#"
fun accept(value: Unit): String = "OK"
fun box(): String = accept(Unit)
"#;
    assert_eq!(run(src, "ResolverSelectedUnitSingleton"), "OK");
}

#[test]
fn implicit_member_beats_top_level_scope_function_for_callable_reference_arguments() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
import kotlin.reflect.KFunction2

class Sample {
    companion object {
        fun max(x: Int, y: Int): Int = if (x > y) x else y
    }
}

abstract class Checker {
    fun check(): String = run(Sample::max) { x, y -> if (x > y) "OK" else "fail" }
    abstract fun <T1, T2, R> run(
        method: KFunction2<T1, T2, R>,
        fn: (T1, T2) -> String,
    ): String
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn implicit_receiver_generic_member_reference_uses_its_expected_shape() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
class Source(private val text: String) {
    inline fun <reified T> read(): T? = text as? T
}

fun use() {
    val read: () -> String? = with(Source("OK")) { ::read }
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn function_values_inherit_any_members_without_a_function_n_name() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
fun <T> renderIdentity(): String = { value: T -> value }.toString()

class Holder<T> {
    fun <R : T> render(value: R): String = (fun(_: List<T>): R = value).toString()
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn dependency_top_level_property_reference_uses_the_imported_symbol_record() {
    const LIBRARY: &str = r#"
package a
var topLevel: Int = 42
val String.extension: Long
    get() = length.toLong()
"#;
    const MAIN: &str = r#"
import a.*

fun use() {
    val reference = ::topLevel
    reference.get()
    reference.set(7)
    val extension = String::extension
    extension.get("abc")
}
"#;
    let Some(diagnostics) =
        common::checker_diags_against("dependency_top_level_property_reference", LIBRARY, MAIN)
    else {
        return;
    };
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn suspend_function_value_invoke_reference_uses_the_function_signature() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
fun capture(block: suspend () -> Unit) {
    val invoke: suspend () -> Unit = block::invoke
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn postponed_builder_receiver_selects_member_callable_reference_by_expected_arity() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
fun use(value: String?) {
    buildList {
        value?.let(::add)
    }
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn nested_generic_builder_constrains_a_constructor_reference() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
data class DataClass(val data: String)

open class Factory<Outer, Field> {
    open fun group(): Factory<Field, String> = TODO()

    fun <Result> apply(
        instance: Factory<Outer, *>,
        function: (Field) -> Result,
    ): Factory<Outer, Result> = TODO()

    companion object {
        fun <T> create(block: (Factory<T, T>) -> Factory<T, T>) {}
    }
}

fun use() {
    Factory.create {
        it.group().apply(it, ::DataClass)
    }
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn bound_value_class_extension_property_reference_uses_its_semantic_receiver() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
@JvmInline value class WrappedInt(val value: Int)
@JvmInline value class Wrapped<T : String>(val value: T)

val WrappedInt.unwrapped get() = value
val Wrapped<String>.unwrapped get() = value

fun use() {
    WrappedInt(42)::unwrapped.get()
    Wrapped("OK")::unwrapped.get()
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn generic_fun_interface_alias_has_an_ordinary_constructor_reference() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
fun interface Transform<Input, Output> {
    fun invoke(value: Input): Output
}

typealias StringTransform<Result> = Transform<String, Result>

class PairWithFixedFirst<First, Second>(val second: Second)
typealias IntFirst<Second> = PairWithFixedFirst<Int, Second>

fun use(function: (String) -> Int) {
    function.let(::StringTransform)
    "value".let(::IntFirst)
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn nullable_continuation_resume_uses_the_stdlib_extension_signature() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
import kotlin.coroutines.*

@JvmInline value class Token(val value: Int)

fun resumeValues(continuation: Continuation<Any>?) {
    continuation?.resume(42)
    continuation?.resume(Token(42))
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn member_result_constrains_a_stdlib_apply_builder() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
class Product
class Builder<C> {
    fun materialize(): C = Product() as C
}

fun <F> build(block: Builder<F>.() -> Unit): Builder<F> = Builder<F>().apply(block)

@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}

fun use() {
    fun consume(value: Product) {}
    val built = build { consume(materialize()) }
    exact<Builder<Product>>(built)
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn constructor_parameter_constrains_a_nested_generic_suspend_result() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
@JvmInline value class Raw(val value: Int)
@JvmInline value class Wrapped(val raw: Raw)

suspend fun <T> produce(): T = TODO()

class Consumer {
    suspend fun <T> pass(value: T): T = value
    suspend fun normalize(value: Wrapped): Wrapped = value

    suspend fun consume(): Wrapped = pass(normalize(pass(Wrapped(produce()))))
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn elvis_preserves_an_applied_common_collection_supertype() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
fun maybeMutable(): MutableList<Int>? = null

fun consume() {
    val target = mutableListOf<Int>()
    val source = maybeMutable() ?: emptyList<Int>()
    target.addAll(source)
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn generic_common_supertypes_reconstruct_kotlin_projections() {
    let source = r#"
class A
class B

class Invariant<T>
class Covariant<out T>
class Contravariant<in T>
class Dependent<A, B : A>
interface Recursive<T>
class RecursiveA : Recursive<RecursiveA>
class RecursiveB : Recursive<RecursiveB>

fun invariant(flag: Boolean): Invariant<out Any> =
    if (flag) Invariant<A>() else Invariant<B>()

fun covariant(flag: Boolean): Covariant<Any> =
    if (flag) Covariant<A>() else Covariant<B>()

fun contravariant(flag: Boolean): Contravariant<Nothing> =
    if (flag) Contravariant<A>() else Contravariant<B>()

fun dependent(flag: Boolean): Dependent<out Any, *> =
    if (flag) Dependent<A, A>() else Dependent<B, B>()

fun recursive(flag: Boolean): Recursive<*> =
    if (flag) RecursiveA() else RecursiveB()
"#;
    let Some((code, diagnostics)) = common::kotlinc_source_result("GenericCommonSupertype", source)
    else {
        return;
    };
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(source),
        Vec::<String>::new()
    );
}

#[test]
fn nullable_assignment_infers_a_non_null_bounded_generic_result() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
fun <T : Any> create(): T = TODO()
var chooseFirst = true

fun select(value: String): String = value
fun select(value: Int): Int = value

fun consume(): String {
    var value: String? = null
    value = if (chooseFirst) create() else error("missing")
    return select(value)
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn function_receiver_extension_joins_a_diverging_lambda_result() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
infix fun <R> (() -> R).recover(alternative: () -> R): R = try {
    this()
} catch (_: Exception) {
    alternative()
}

fun consume(): String = {
    error("missing")
} recover {
    "OK"
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn overloaded_method_type_parameter_bounds_select_the_applicable_declaration() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"
open class Marker

class Select {
    fun <T> choose(value: T): String = "any"
    fun <T : Marker> choose(value: T): String = "marker"
}

fun consume(): String {
    val select = Select()
    val unconstrained = select.choose("OK")
    val constrained = select.choose(Marker())
    return unconstrained + constrained
}
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn indexed_assignment_provides_the_value_lambda_shape() {
    let src = r#"
object Sink {
    var result = ""

    operator fun set(key: String, produce: ((String) -> Unit) -> Unit) {
        result += key
        produce { result += it }
    }
}

fun box(): String {
    Sink["O"] = { it("K") }
    return Sink.result
}
"#;
    assert_eq!(run(src, "ResolverIndexedAssignmentLambda"), "OK");
}

#[test]
fn nested_constructor_uses_its_recorded_source_declaration() {
    common::expect_box_ok_files_with_stdlib(
        &[
            (
                "Model.kt",
                "package model\nclass Outer { class Nested(val value: String) }",
            ),
            (
                "Main.kt",
                "import model.Outer\nfun box(): String = Outer.Nested(\"OK\").value",
            ),
        ],
        "ResolverNestedSourceDeclaration",
    );
}

#[test]
fn inner_constructor_and_its_typealias_bind_the_value_receiver() {
    let src = r#"
class Outer(val prefix: String) {
    inner class Inner(val suffix: String) {
        val value = prefix + suffix
    }
}
typealias InnerAlias = Outer.Inner

fun box(): String {
    val outer = Outer("O")
    val direct = outer.Inner("K").value
    val aliased = outer.InnerAlias("K").value
    return if (direct == "OK" && aliased == "OK") "OK" else "$direct/$aliased"
}
"#;
    assert_eq!(run(src, "ResolverInnerConstructorAlias"), "OK");
}

#[test]
fn enabled_when_guard_uses_the_type_test_scope_and_short_circuits() {
    let src = r#"
// LANGUAGE: +WhenGuards
sealed interface Value
class Text(val text: String) : Value
class NumberValue : Value

fun render(value: Value): String = when (value) {
    is Text if value.text.isEmpty() -> "empty"
    is Text -> value.text
    else -> "number"
}

fun box(): String {
    if (render(Text("")) != "empty") return "empty"
    if (render(Text("OK")) != "OK") return "text"
    return if (render(NumberValue()) == "number") "OK" else "number"
}
"#;
    assert_eq!(run(src, "ResolverWhenGuard"), "OK");
}

#[test]
fn lambda_return_overload_stays_separate_from_normal_inline_hofs() {
    let src = r#"
fun box(): String {
    val s = listOf(1, 2, 3).sumOf { it * 2 }
    if (s != 12) return "sumOf=$s"
    var total = 0
    listOf(1, 2, 3).forEach { total += it }
    if (total != 6) return "forEach=$total"
    val mapped = listOf(1, 2, 3).map { it * 10 }
    if (mapped != listOf(10, 20, 30)) return "map=$mapped"
    val folded = listOf("a", "b", "c").fold("") { acc, x -> acc + x }
    return if (folded == "abc") "OK" else "fold=$folded"
}
"#;
    let out = run(src, "ResolverInlineHofs");
    assert_eq!(out, "OK");
}

/// A metadata-resolved `toUShort()` returns the narrow unsigned value class, and its `toString`
/// prints the UNSIGNED decimal — `40000` is the `short` `-25536` in the representation.
#[test]
fn unsigned_narrow_metadata_return_round_trips() {
    let src = r#"
fun box(): String {
    val x = 40000.toUShort()
    return if (x.toString() == "40000") "OK" else "x=$x"
}
"#;
    let out = run(src, "ResolverUnsignedReturn");
    assert_eq!(out, "OK");
}

#[test]
fn unsigned_integral_conversions_resolve_from_metadata() {
    let src = r#"
fun box(): String {
    val u = 42.toUInt()
    if (u.toInt() != 42) return "u=$u"
    val ul = 42L.toULong()
    if (ul.toLong() != 42L) return "ul=$ul"
    return "OK"
}
"#;
    let out = run(src, "ResolverUnsignedIntegralConversions");
    assert_eq!(out, "OK");
}

#[test]
fn unsigned_binary_operators_use_library_type_identity() {
    let src = r#"
fun box(): String {
    val u = 40.toUInt() + 2.toUInt()
    if (u.toInt() != 42) return "u=$u"
    if (!(u > 1.toUInt())) return "cmp"
    val l = 40L.toULong() + 2L.toULong()
    if (l.toLong() != 42L) return "l=$l"
    return "OK"
}
"#;
    let out = run(src, "ResolverUnsignedBinaryOperators");
    assert_eq!(out, "OK");
}

#[test]
fn anonymous_object_keeps_enclosing_function_type_params() {
    let src = r#"
interface Sink<T> {
    fun take(value: T)
}

fun <T> makeSink(): Any = object : Sink<T> {
    override fun take(value: T) {}
}

fun box(): String {
    makeSink<String>()
    return "OK"
}
"#;
    let out = run(src, "ResolverAnonObjectGenericScope");
    assert_eq!(out, "OK");
}

#[test]
fn property_first_and_extension_first_call_do_not_collide() {
    let src = r#"
fun box(): String {
    val r = 0..3
    if (r.first != 0) return "range property=${r.first}"
    if (r.first() != 0) return "range call=${r.first()}"

    val xs = listOf(7, 8)
    if (xs.first() != 7) return "list call=${xs.first()}"
    if (xs.size != 2) return "list size=${xs.size}"
    return "OK"
}
"#;
    let out = run(src, "ResolverFirstPropertyVsCall");
    assert_eq!(out, "OK");
}

#[test]
fn primitive_builtin_infix_extension_source_form_matters() {
    let java_home = common::java_home();
    let stdlib = common::stdlib_jar();
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
infix fun Int.rem(other: Int) = 10
infix operator fun Int.minus(other: Int): Int = 20

fun box(): String {
    val a = 5 rem 2
    if (a != 10) return "fail 1"

    val b = 5 minus 3
    if (b != 20) return "fail 2"

    val a1 = 5.rem(2)
    if (a1 != 1) return "fail 3"

    val b2 = 5.minus(3)
    if (b2 != 2) return "fail 4"

    return "OK"
}
"#;
    let out =
        common::compile_and_run_box(src, "PrimitiveBuiltinInfixAmbiguity", &[stdlib], Some(&jdk));
    assert_eq!(out.as_deref(), Some("OK"));
}
