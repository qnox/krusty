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
fn nested_sealed_interface_keeps_its_exhaustive_subclass_tree() {
    // `WithValue` must remain sealed so exhaustiveness traversal reaches its `Step` subclass.
    let src = r#"
sealed interface G<out T> {
    data object Empty : G<Nothing>
    sealed interface WithValue<T> : G<T> { val value: T }
    data class Step<T>(override val value: T) : WithValue<T>
}
fun <R> G<R>.describe(): Int = when (this) {
    is G.Step<*> -> 1
    is G.Empty -> 2
}
fun box(): String = if (G.Step("x").describe() == 1) "OK" else "FAIL"
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ResolverNestedSealedInterface", src);
    assert_eq!((code, diagnostics.as_str()), (0, ""));
    assert_eq!(run(src, "ResolverNestedSealedInterface"), "OK");
}

#[test]
fn selected_generic_member_keeps_its_inferred_return() {
    let src = r#"
class C {
    fun <T> id(value: T) = value
    fun use() = id("OK").length
}
fun box(): String = if (C().use() == 2) "OK" else "FAIL"
"#;
    assert_eq!(run(src, "ResolverSelectedGenericMemberReturn"), "OK");
}

#[test]
fn chained_generic_extension_call_types_an_inferred_property() {
    let src = r#"
val values = listOf("a", "b").asSequence()
fun use() = values.withIndex()
"#;
    common::expect_true_e2e(
        "chained_generic_extension_call_types_an_inferred_property",
        src,
        &[],
    );
}

#[test]
fn extension_on_a_classpath_supertype_applies_to_its_implementation() {
    let src = r#"
fun indexed(builder: StringBuilder) = builder.withIndex()
"#;
    common::expect_true_e2e(
        "extension_on_a_classpath_supertype_applies_to_its_implementation",
        src,
        &[],
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
    common::expect_true_e2e(
        "context_typed_lambda_selects_the_callable_for_property_inference",
        src,
        &[],
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
    // BACKEND STILL BAILS on this shape (a generic member-extension property over a fun-type
    // receiver): checker-clean is asserted, emission is a known gap — upgrade to
    // `expect_true_e2e` when the member-ext-property gate admits it.
    // BACKEND STILL BAILS on this shape: checker-clean is asserted, emission is a known
    // gap - upgrade to `expect_true_e2e` when the backend admits it.
    let bail_diags = common::front_end_diagnostics_with_stdlib(src);
    assert!(bail_diags.is_empty(), "{bail_diags:?}");
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
fn empty_array_argument_uses_java_platform_parameter_element_type() {
    // The flexible `Array<String>!` parameter fixes the empty array's element type.
    let src = r#"
import java.util.Optional
fun f(o: Optional<Array<String>>): Int = o.orElse(emptyArray()).size
fun box(): String = if (f(Optional.of(arrayOf("x"))) == 1) "OK" else "FAIL"
"#;
    assert_eq!(run(src, "ResolverEmptyArrayJavaPlatformParam"), "OK");
}

#[test]
fn empty_array_with_a_primitive_element_keeps_the_boxed_array_type() {
    let src = r#"
import java.util.Optional
fun box(): String {
    val values: Array<Int> = Optional.empty<Array<Int>>().orElse(emptyArray())
    return if (values.javaClass.componentType.name == "java.lang.Integer") "OK" else "FAIL"
}
"#;
    assert_eq!(run(src, "ResolverEmptyArrayBoxedPrimitive"), "OK");
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
    common::expect_true_e2e(
        "core_any_constructor_is_an_ordinary_candidate_without_a_classpath",
        "fun make(): Any = Any()",
        &[],
    );
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
fn suppressed_invisible_dependency_overload_finalizes_an_inferred_signature() {
    let library = common::compile_lib(
        "suppressed_invisible_dependency",
        r#"package hidden
internal fun choose(value: String) = "wrong"
internal fun choose(value: String, marker: Any) = "OK"
"#,
    )
    .expect("compile dependency");
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let diagnostics = common::front_end_diagnostics(
        r#"import hidden.choose
@Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")
fun box() = choose("", Any())
"#,
        &[library, stdlib],
        Some(jdk.as_path()),
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn signed_unary_constants_use_the_nullable_primitive_expectation() {
    let diagnostics = common::front_end_diagnostics(
        r#"fun values() {
    val byte: Byte? = -1
    val byteMin: Byte = -128
    val short: Short? = +1
    val shortMin: Short = -32768
}
"#,
        &[],
        None,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn flow_narrowed_primitive_can_still_be_compared_with_null() {
    let diagnostics = common::front_end_diagnostics(
        r#"fun decrement(value: Byte?): Byte? {
    var current = value
    if (current != null) current--
    return current
}
"#,
        &[],
        None,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn generic_elvis_joins_through_the_definitely_non_null_upper_bound() {
    let diagnostics = common::front_end_diagnostics_with_stdlib(
        r#"fun <T : Number?> choose(value: T) = value ?: 42
fun consume(value: Number?): Int = (value ?: 42).toInt()
fun use(): Int = choose<Int?>(null).toInt()
"#,
    );
    assert_eq!(diagnostics, Vec::<String>::new());
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
    common::expect_true_e2e(
        "suspend_coroutine_intrinsic_uses_its_selected_stdlib_signature",
        r#"
import kotlin.coroutines.intrinsics.*
suspend fun suspendForever(): Int =
    suspendCoroutineUninterceptedOrReturn { COROUTINE_SUSPENDED }
"#,
        &[],
    );
}

#[test]
fn suspend_coroutine_intrinsic_preserves_its_continuation_type() {
    common::expect_true_e2e(
        "suspend_coroutine_intrinsic_preserves_its_continuation_type",
        r#"
import kotlin.coroutines.resume
import kotlin.coroutines.intrinsics.*
suspend fun <T> await(value: T): T =
    suspendCoroutineUninterceptedOrReturn { continuation ->
        continuation.resume(value)
        COROUTINE_SUSPENDED
    }
"#,
        &[],
    );
}

#[test]
fn nested_contextual_result_preserves_the_lambda_input_type_parameter() {
    let source = r#"
interface Marker { fun mark(): String }

fun <T> build(transform: (T) -> String): List<T> = TODO()
fun <U : Marker> outer(): List<U> = build { it.mark() }
"#;
    let (code, diagnostics) = common::kotlinc_source_result("NestedContextResult", source);
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    common::expect_true_e2e(
        "nested_contextual_result_preserves_the_lambda_input_type_parameter",
        source,
        &[],
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
    let (code, diagnostics) = common::kotlinc_source_result("RepeatedContextResult", source);
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
    let (code, diagnostics) = common::kotlinc_source_result("MixedContextResult", source);
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
    let (code, diagnostics) = common::kotlinc_source_result("NullableNestedContext", source);
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
    common::expect_true_e2e(
        "implicit_member_beats_top_level_scope_function_for_callable_reference_arguments",
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
        &[],
    );
}

#[test]
fn implicit_receiver_generic_member_reference_uses_its_expected_shape() {
    common::expect_true_e2e(
        "implicit_receiver_generic_member_reference_uses_its_expected_shape",
        r#"
class Source(private val text: String) {
    inline fun <reified T> read(): T? = text as? T
}

fun use() {
    val read: () -> String? = with(Source("OK")) { ::read }
}
"#,
        &[],
    );
}

#[test]
fn function_values_inherit_any_members_without_a_function_n_name() {
    common::expect_true_e2e(
        "function_values_inherit_any_members_without_a_function_n_name",
        r#"
fun <T> renderIdentity(): String = { value: T -> value }.toString()

class Holder<T> {
    fun <R : T> render(value: R): String = (fun(_: List<T>): R = value).toString()
}
"#,
        &[],
    );
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
    let diagnostics =
        common::checker_diags_against_ref("dependency_top_level_property_reference", LIBRARY, MAIN)
            .expect("reference compiler unavailable");
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn suspend_function_value_invoke_reference_uses_the_function_signature() {
    // BACKEND STILL BAILS on this shape: checker-clean is asserted, emission is a known
    // gap - upgrade to `expect_true_e2e` when the backend admits it.
    let bail_diags = common::front_end_diagnostics_with_stdlib(
        r#"
fun capture(block: suspend () -> Unit) {
    val invoke: suspend () -> Unit = block::invoke
}
"#,
    );
    assert!(bail_diags.is_empty(), "{bail_diags:?}");
}

#[test]
fn postponed_builder_receiver_selects_member_callable_reference_by_expected_arity() {
    common::expect_true_e2e(
        "postponed_builder_receiver_selects_member_callable_reference_by_expected_arity",
        r#"
fun use(value: String?) {
    buildList {
        value?.let(::add)
    }
}
"#,
        &[],
    );
}

#[test]
fn nested_generic_builder_constrains_a_constructor_reference() {
    common::expect_true_e2e(
        "nested_generic_builder_constrains_a_constructor_reference",
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
        &[],
    );
}

#[test]
fn bound_value_class_extension_property_reference_uses_its_semantic_receiver() {
    common::expect_true_e2e(
        "bound_value_class_extension_property_reference_uses_its_semantic_receiver",
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
        &[],
    );
}

#[test]
fn generic_fun_interface_alias_has_an_ordinary_constructor_reference() {
    // BACKEND STILL BAILS on this shape: checker-clean is asserted, emission is a known
    // gap - upgrade to `expect_true_e2e` when the backend admits it.
    let bail_diags = common::front_end_diagnostics_with_stdlib(
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
    assert!(bail_diags.is_empty(), "{bail_diags:?}");
}

#[test]
fn nullable_continuation_resume_uses_the_stdlib_extension_signature() {
    common::expect_true_e2e(
        "nullable_continuation_resume_uses_the_stdlib_extension_signature",
        r#"
import kotlin.coroutines.*

@JvmInline value class Token(val value: Int)

fun resumeValues(continuation: Continuation<Any>?) {
    continuation?.resume(42)
    continuation?.resume(Token(42))
}
"#,
        &[],
    );
}

#[test]
fn member_result_constrains_a_stdlib_apply_builder() {
    common::expect_true_e2e(
        "member_result_constrains_a_stdlib_apply_builder",
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
        &[],
    );
}

#[test]
fn constructor_parameter_constrains_a_nested_generic_suspend_result() {
    // BACKEND STILL BAILS on this shape: checker-clean is asserted, emission is a known
    // gap - upgrade to `expect_true_e2e` when the backend admits it.
    let bail_diags = common::front_end_diagnostics_with_stdlib(
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
    assert!(bail_diags.is_empty(), "{bail_diags:?}");
}

#[test]
fn elvis_preserves_an_applied_common_collection_supertype() {
    common::expect_true_e2e(
        "elvis_preserves_an_applied_common_collection_supertype",
        r#"
fun maybeMutable(): MutableList<Int>? = null

fun consume() {
    val target = mutableListOf<Int>()
    val source = maybeMutable() ?: emptyList<Int>()
    target.addAll(source)
}
"#,
        &[],
    );
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
    let (code, diagnostics) = common::kotlinc_source_result("GenericCommonSupertype", source);
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    common::expect_true_e2e(
        "generic_common_supertypes_reconstruct_kotlin_projections",
        source,
        &[],
    );
}

#[test]
fn nullable_assignment_infers_a_non_null_bounded_generic_result() {
    common::expect_true_e2e(
        "nullable_assignment_infers_a_non_null_bounded_generic_result",
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
        &[],
    );
}

#[test]
fn function_receiver_extension_joins_a_diverging_lambda_result() {
    common::expect_true_e2e(
        "function_receiver_extension_joins_a_diverging_lambda_result",
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
        &[],
    );
}

#[test]
fn overloaded_method_type_parameter_bounds_select_the_applicable_declaration() {
    common::expect_true_e2e(
        "overloaded_method_type_parameter_bounds_select_the_applicable_declaration",
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
        &[],
    );
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
fn anonymous_object_method_keeps_captured_function_parameter_type_identity() {
    let src = r#"
interface Sink<T> {
    fun take(value: T)
}

fun <T> makeSink(action: (T) -> Unit): Sink<T> = object : Sink<T> {
    override fun take(value: T) {
        action(value)
    }
}

fun box(): String {
    var result = "FAIL"
    makeSink<String> { result = it }.take("OK")
    return result
}
"#;
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("ResolverAnonObjectCapturedGenericReference", src);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected generic anonymous-object capture: {reference_stderr}"
    );
    assert_eq!(run(src, "ResolverAnonObjectCapturedGenericIdentity"), "OK");
}

#[test]
fn anonymous_object_uses_the_nearest_shadowing_type_parameter_identity() {
    let src = r#"
interface Sink<T> {
    fun take(value: T)
}

class Outer<T> {
    fun <T> makeSink(action: (T) -> Unit): Sink<T> = object : Sink<T> {
        override fun take(value: T) {
            action(value)
        }
    }
}

fun box(): String {
    var result = "FAIL"
    Outer<Int>().makeSink<String> { result = it }.take("OK")
    return result
}
"#;
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("ResolverAnonObjectNearestShadowedGenericReference", src);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected the shadowed anonymous-object capture: {reference_stderr}"
    );
    assert_eq!(
        run(src, "ResolverAnonObjectNearestShadowedGenericIdentity"),
        "OK"
    );
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
