//! Focused classpath resolver regressions. These duplicate a few cases from the larger feature bundle so
//! resolver/provider cleanup gets a small, direct failure when metadata or inline-overload selection drifts.

use std::path::Path;

use super::common;

#[derive(Debug, PartialEq, Eq)]
struct ObservedDiagnostic {
    file: String,
    line: usize,
    column: usize,
    message: String,
}

/// kotlinc's message for the single error of the running test's `tag` fixture. 2.4.20 names the
/// receiver's type where `x.length` did not smart-cast.
fn recorded_message(tag: &str, kotlinc_output: &str) -> String {
    let errors = reference_errors(kotlinc_output);
    assert_eq!(errors.len(), 1, "{tag}: {kotlinc_output}");
    errors[0].message.clone()
}

fn reference_errors(output: &str) -> Vec<ObservedDiagnostic> {
    let lines = output.lines().collect::<Vec<_>>();
    let (diagnostics, remainder) = lines.as_chunks::<3>();
    assert_eq!(remainder.len(), 0);
    diagnostics
        .iter()
        .map(|diagnostic| {
            let line = diagnostic[0];
            let (location, message) = line
                .split_once(": error: ")
                .unwrap_or_else(|| panic!("unexpected kotlinc diagnostic: {line}"));
            let mut fields = location.rsplitn(3, ':');
            let column = fields
                .next()
                .expect("kotlinc diagnostic column")
                .parse()
                .expect("numeric kotlinc diagnostic column");
            let line = fields
                .next()
                .expect("kotlinc diagnostic line")
                .parse()
                .expect("numeric kotlinc diagnostic line");
            let path = fields.next().expect("kotlinc diagnostic path");
            ObservedDiagnostic {
                file: Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("UTF-8 kotlinc diagnostic filename")
                    .to_string(),
                line,
                column,
                message: message.to_string(),
            }
        })
        .collect()
}

/// Reject one fixture exactly as the selected kotlinc does. The reference stderr is replayed from
/// the binary GHA recording for a published compiler version, so removing the old checked-in text
/// ledger must not weaken this to a non-empty/rejection-only assertion.
fn assert_rejection_same_as_kotlinc(stem: &str, source: &str) {
    let file = format!("{stem}.kt");
    let result = common::compiler_diagnostics(
        &[(file.as_str(), source)],
        std::slice::from_ref(&common::stdlib_jar()),
    );
    common::expect_identical_rejection(&result, stem);
}

/// Strict stdlib/JDK run: missing tooling or a rejected source panics with diagnostics, so callers
/// cannot turn either failure into a passing skip.
fn run(src: &str, stem: &str) -> String {
    common::expect_box_run_with_stdlib(src, stem)
}

fn assert_frontends_accept(sources: &[(&str, &str)]) {
    let result = common::compiler_diagnostics(sources, &[common::stdlib_jar()]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, "")
    );
    let source_text = sources
        .iter()
        .map(|(_, source)| *source)
        .collect::<Vec<_>>();
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&source_text),
        Vec::<String>::new()
    );
}

fn assert_accepted_and_runs(src: &str, stem: &str) {
    let (reference_code, reference_diagnostics) = common::kotlinc_source_result(stem, src);
    assert_eq!((reference_code, reference_diagnostics.as_str()), (0, ""));
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        Vec::<String>::new()
    );
    assert_eq!(run(src, stem), "OK");
}

fn assert_accepted_and_runs_files(sources: &[(&str, &str)], stem: &str) {
    assert_frontends_accept(sources);
    common::expect_box_ok_files_with_stdlib(sources, stem);
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
fn companion_anonymous_object_does_not_capture_outer_type_parameter() {
    let src = r#"
class Attempt<T> private constructor(private val value: T) {
    companion object {
        fun <T> tryAgain(): Attempt<T> {
            @Suppress("UNCHECKED_CAST")
            return Attempt(tryAgain) as Attempt<T>
        }
        private val tryAgain: Any = object {
            override fun toString(): String = "try again"
        }
    }
}
fun box(): String {
    Attempt.tryAgain<String>()
    return "OK"
}
"#;
    assert_accepted_and_runs(src, "ResolverAnonObjectCompanionScope");
}

#[test]
fn plain_nested_class_anonymous_object_does_not_capture_outer_type_parameter() {
    let src = r#"
class Outer<T>(val value: T) {
    class Nested {
        val marker = object { override fun toString() = "nested" }
    }
}
fun box(): String = if (Outer.Nested().marker.toString() == "nested") "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverAnonObjectNestedClassScope");
}

#[test]
fn local_classifier_shadows_type_parameter_for_anonymous_object() {
    let src = r#"
fun <T> build(): Any {
    class T
    return object { override fun toString() = "shadowed" }
}
fun box(): String = if (build<String>().toString() == "shadowed") "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverAnonObjectLocalClassifierScope");
}

#[test]
fn constructor_call_binds_unconstrained_type_parameter_from_expected_return() {
    let src = r#"
interface MyMap<K, V> { fun describe(): String }
class MapImpl<K, E, V>(val f: Factory<K, E>) : MyMap<K, V> {
    override fun describe(): String = "OK"
}
interface Factory<K, E> {
    fun <V> createMap(): MyMap<K, V> = MapImpl(this)
}
class StrFactory : Factory<String, Char>
fun box(): String = StrFactory().createMap<Int>().describe()
"#;
    let (code, diagnostics) =
        common::kotlinc_source_result("ResolverCtorExpectedTypeBindingReference", src);
    assert_eq!((code, diagnostics), (0, String::new()));
    assert_eq!(run(src, "ResolverCtorExpectedTypeBinding"), "OK");
}

#[test]
fn classpath_constructor_binds_unconstrained_type_parameter_from_expected_return() {
    let library = r#"
package lib
interface MyMap<K, V> { fun describe(): String }
class MapImpl<K, E, V>(val factory: Factory<K, E>) : MyMap<K, V> {
    override fun describe(): String = "OK"
}
interface Factory<K, E>
class StrFactory : Factory<String, Char>
"#;
    let main = r#"
import lib.Factory
import lib.MapImpl
import lib.MyMap
import lib.StrFactory
fun create(factory: Factory<String, Char>): MyMap<String, Int> = MapImpl(factory)
fun box(): String = create(StrFactory()).describe()
"#;
    assert_eq!(
        common::expect_box_run_against_kotlinc(library, main).as_deref(),
        Some("OK")
    );
}

#[test]
fn constructor_call_contradicting_the_expected_type_still_errors() {
    let src = r#"
interface MyMap<K, V>
class MapImpl<K, E, V>(val f: Factory<K, E>) : MyMap<K, V>
interface Factory<K, E>
class StrFactory : Factory<String, Char>
val x: MyMap<String, Int> = MapImpl<Char, Char, String>(StrFactory())
"#;
    let (code, diagnostics) = common::kotlinc_source_result("CtorExpectedContradiction", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [
            ObservedDiagnostic {
                file: "CtorExpectedContradiction.kt".to_string(),
                line: 6,
                column: 27,
                message: "initializer type mismatch: expected 'MyMap<String, Int>', actual 'MapImpl<Char, Char, String>'.".to_string(),
            },
            ObservedDiagnostic {
                file: "CtorExpectedContradiction.kt".to_string(),
                line: 6,
                column: 57,
                message: "argument type mismatch: actual type is 'StrFactory', but 'Factory<Char, Char>' was expected.".to_string(),
            },
        ]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [
            "argument type mismatch: actual type is 'StrFactory', but 'Factory<Char, Char>' was expected."
                .to_string(),
            "initializer type mismatch: expected 'MyMap<String, Int>', actual 'MapImpl<Char, Char, String>'."
                .to_string(),
        ]
    );
}

#[test]
fn constructor_expected_return_does_not_hide_an_unconstrained_parameter() {
    let src = r#"
interface MyMap<K, V>
class MapImpl<K, E, V>(val key: K) : MyMap<K, V>
val x: MyMap<String, Int> = MapImpl("value")
"#;
    let (code, diagnostics) = common::kotlinc_source_result("CtorExpectedPartial", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "CtorExpectedPartial.kt".to_string(),
            line: 4,
            column: 29,
            message: "cannot infer type for type parameter 'E'. Specify it explicitly.".to_string(),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        ["initializer type mismatch: expected 'MyMap<String, Int>', actual 'MapImpl<String, Any?, Any?>'."
            .to_string()]
    );
}

#[test]
fn constructor_expected_return_respects_type_parameter_bounds() {
    let src = r#"
interface View<V>
class TextImpl<V : CharSequence> : View<V>
val x: View<Int> = TextImpl()
"#;
    let (code, diagnostics) = common::kotlinc_source_result("CtorExpectedBound", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "CtorExpectedBound.kt".to_string(),
            line: 4,
            column: 20,
            message: "cannot infer type for type parameter 'V'. Specify it explicitly.".to_string(),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [
            "initializer type mismatch: expected 'View<Int>', actual 'TextImpl<CharSequence>'."
                .to_string()
        ]
    );
}

#[test]
fn member_delegated_property_keeps_the_delegate_value_type() {
    let src = r#"
class C(val count: Int) {
    val h by lazy { 31 * count }
    fun use(): Int = h
}
fun box(): String = if (C(7).use() == 217) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverMemberDelegatedProperty");
}

#[test]
fn member_delegated_property_with_explicit_lazy_mode_keeps_the_delegate_value_type() {
    let src = r#"
class C(val count: Int) {
    val h by lazy(LazyThreadSafetyMode.PUBLICATION) { 31 * count }
    fun use(): Int = h
}
fun box(): String = if (C(7).use() == 217) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverMemberDelegatedLazyMode");
}

#[test]
fn as_cast_in_condition_smart_casts_on_fallthrough() {
    let src = r#"
fun f(a: Any?, len: Int): Boolean {
    if (a == null || len != (a as CharSequence).length) return false
    return a.length > 0
}
fun box(): String = if (f("abc", 3)) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverAsConditionSmartCast");
}

#[test]
fn safe_as_cast_in_condition_does_not_smart_cast_on_fallthrough() {
    let src = r#"
fun g(a: Any?, len: Int): Boolean {
    if (a == null || len != ((a as? CharSequence)?.length ?: -1)) return false
    return a.length > 0
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ResolverSafeAsCondition", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "ResolverSafeAsCondition.kt".to_string(),
            line: 4,
            column: 14,
            message: recorded_message("ResolverSafeAsCondition", &diagnostics),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [recorded_message("ResolverSafeAsCondition", &diagnostics)]
    );
}

#[test]
fn conditionally_evaluated_as_cast_does_not_smart_cast() {
    let src = r#"
fun f(a: Any?, skip: Boolean): Int {
    if (skip || (a as CharSequence).length == 0) return a.length
    return 0
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ResolverConditionalAs", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "ResolverConditionalAs.kt".to_string(),
            line: 3,
            column: 59,
            message: recorded_message("ResolverConditionalAs", &diagnostics),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [recorded_message("ResolverConditionalAs", &diagnostics)]
    );
}

#[test]
fn closure_mutated_value_does_not_smart_cast_after_as_cast() {
    let src = r#"
fun f(value: Any?): Int {
    var current = value
    val clear = { current = null }
    if ((current as CharSequence).length == 0) return 0
    return current.length
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ResolverUnstableAs", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "ResolverUnstableAs.kt".to_string(),
            line: 6,
            column: 20,
            message: recorded_message("ResolverUnstableAs", &diagnostics),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [recorded_message("ResolverUnstableAs", &diagnostics)]
    );
}

#[test]
fn nullable_target_as_cast_narrows_to_the_nullable_target() {
    let src = r#"
fun f(a: Any?): Int {
    if ((a as CharSequence?) == null) return -1
    return a.length
}
fun box(): String = if (f("abc") == 3 && f(null) == -1) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverNullableTargetAs");
}

#[test]
fn as_cast_in_third_disjunct_smart_casts_on_fallthrough() {
    let src = r#"
fun f(value: Any?, blocked: Boolean): Int {
    if (value == null || blocked || (value as CharSequence).length == 0) return 0
    return value.length
}
fun box(): String = if (f("abc", false) == 3) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverAsConditionThirdDisjunct");
}

#[test]
fn as_cast_in_nested_if_condition_smart_casts_on_both_outcomes() {
    let src = r#"
fun f(value: Any): Boolean {
    if (if (value as Boolean) true else false) return value
    return value
}
fun box(): String = if (f(true) && !f(false)) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverAsNestedIfCondition");
}

#[test]
fn bare_as_statement_smart_casts_on_fallthrough() {
    let src = r#"
interface Named { val data: String }
class N(override val data: String) : Named
fun f(other: Any): String {
    other as Named
    return other.data
}
fun box(): String = if (f(N("ok")) == "ok") "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverAsStatementSmartCast");
}

#[test]
fn nullable_target_as_statement_keeps_its_proven_type() {
    let src = r#"
@Suppress("UNUSED_EXPRESSION")
fun f(value: Any?): Int {
    value as CharSequence?
    if (value == null) return -1
    return value.length
}
fun box(): String = if (f("abc") == 3 && f(null) == -1) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverNullableAsStatement");
}

#[test]
fn as_cast_in_evaluated_call_argument_smart_casts_after_statement() {
    let src = r#"
fun consume(value: Int) {}
fun f(value: Any): Int {
    consume((value as CharSequence).length)
    return value.length
}
fun box(): String = if (f("abc") == 3) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ResolverCallArgumentAsStatement");
}

#[test]
fn safe_as_statement_does_not_smart_cast() {
    let src = r#"
@Suppress("UNUSED_EXPRESSION")
fun f(value: Any?): Int {
    value as? CharSequence
    return value.length
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ResolverSafeAsStatement", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "ResolverSafeAsStatement.kt".to_string(),
            line: 5,
            column: 18,
            message: recorded_message("ResolverSafeAsStatement", &diagnostics),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [recorded_message("ResolverSafeAsStatement", &diagnostics)]
    );
}

#[test]
fn conditional_as_in_statement_does_not_smart_cast() {
    let src = r#"
@Suppress("UNUSED_EXPRESSION")
fun f(value: Any?, skip: Boolean): Int {
    skip || (value as CharSequence).isEmpty()
    return value.length
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ResolverConditionalAsStatement", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "ResolverConditionalAsStatement.kt".to_string(),
            line: 5,
            column: 18,
            message: recorded_message("ResolverConditionalAsStatement", &diagnostics),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [recorded_message(
            "ResolverConditionalAsStatement",
            &diagnostics
        )]
    );
}

#[test]
fn closure_mutated_value_does_not_smart_cast_after_as_statement() {
    let src = r#"
@Suppress("UNUSED_EXPRESSION")
fun f(value: Any?): Int {
    var current = value
    val clear = { current = null }
    current as CharSequence
    return current.length
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ResolverUnstableAsStatement", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "ResolverUnstableAsStatement.kt".to_string(),
            line: 7,
            column: 20,
            message: recorded_message("ResolverUnstableAsStatement", &diagnostics),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [recorded_message(
            "ResolverUnstableAsStatement",
            &diagnostics
        )]
    );
}

#[test]
fn member_call_on_class_typed_receiver_substitutes_both_class_and_member_parameters() {
    let src = r#"
interface MyMap<K, V> {
    fun descendantKeys(key: K): Set<K>
}

fun interface MyFactory<K, E> {
    fun convert(element: K): List<E>

    fun <V> createMap(): MyMap<K, V> = TODO()
}

class SetImpl<K, E>(convertor: MyFactory<K, E>) {
    private val map = convertor.createMap<Nothing?>()

    fun descendants(element: K): Set<K> {
        return map.descendantKeys(element)
    }
}
"#;
    assert_frontends_accept(&[("MixedClassMemberParams.kt", src)]);
}

#[test]
fn inferred_member_properties_keep_direct_and_nested_classifier_parameters() {
    const DECLARATIONS: &str = r#"
interface Values<T> {
    fun direct(): T
    fun nested(): List<T>
}

class Holder<T>(values: Values<T>) {
    val direct = values.direct()
    val nested = values.nested()
}

class StringValues : Values<String> {
    override fun direct(): String = "OK"
    override fun nested(): List<String> = listOf("OK")
}
"#;
    const USE: &str = r#"
fun box(): String {
    val holder = Holder<String>(StringValues())
    val direct: String = holder.direct
    val nested: List<String> = holder.nested
    return if (direct == "OK" && nested.single() == "OK") "OK" else "FAIL"
}
"#;

    assert_accepted_and_runs_files(
        &[("Declarations.kt", DECLARATIONS), ("Use.kt", USE)],
        "InferredDirectAndNestedProperties",
    );
}

#[test]
fn inferred_member_property_erasure_respects_classifier_bounds() {
    let source = r#"
interface Producer<T> {
    fun produce(): T
}

class ReferenceBound<T : CharSequence>(producer: Producer<T>) {
    private val value = producer.produce()

    fun current(): T = value
    fun length(): Int = value.length
}

class NullableBound<T : CharSequence?>(producer: Producer<T>) {
    private val value = producer.produce()

    fun current(): T = value
}

class StringProducer : Producer<String> {
    override fun produce(): String = "OK"
}

class NullableProducer : Producer<String?> {
    override fun produce(): String? = null
}

fun box(): String {
    val reference = ReferenceBound<String>(StringProducer())
    val nullable = NullableBound<String?>(NullableProducer())
    return if (reference.length() == 2 && nullable.current() == null) "OK" else "FAIL"
}
"#;
    assert_accepted_and_runs(source, "InferredPropertyBounds");
}

#[test]
fn inferred_direct_nullable_parameter_property_round_trips() {
    let source = r#"
interface Values<T> {
    fun nullable(): T?
}

class IntValues : Values<Int> {
    override fun nullable(): Int? = 41
}

class Holder<T>(values: Values<T>) {
    private val value = values.nullable()

    fun current(): T? = value
}

fun box(): String {
    val value: Int? = Holder<Int>(IntValues()).current()
    return if (value == 41) "OK" else "FAIL"
}
"#;
    assert_accepted_and_runs(source, "InferredNullableParameterProperty");
}

#[test]
fn inferred_member_property_keeps_a_captured_outer_parameter() {
    let source = r#"
interface Producer<T> { fun produce(): T }

class Outer<T> {
    inner class Holder(producer: Producer<T>) {
        private val value = producer.produce()

        fun current(): T = value
    }
}

fun read(holder: Outer<String>.Holder): String = holder.current()
"#;
    assert_frontends_accept(&[("InferredCapturedParameterProperty.kt", source)]);
}

#[test]
fn default_imported_const_val_is_a_top_level_property() {
    let source = r#"
fun read(bufferSize: Int = DEFAULT_BUFFER_SIZE): Int = bufferSize

fun box(): String = if (read() == 8192) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(source, "DefaultImportedConstVal");
}

#[test]
fn imported_const_val_alias_selects_the_constant_property() {
    let source = r#"
import kotlin.io.DEFAULT_BUFFER_SIZE as BUFFER_SIZE

fun box(): String = if (BUFFER_SIZE == 8192) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(source, "ImportedConstValAlias");
}

#[test]
fn qualified_const_val_selects_the_constant_property() {
    let source = r#"
fun box(): String = if (kotlin.io.DEFAULT_BUFFER_SIZE == 8192) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(source, "QualifiedConstVal");
}

#[test]
fn non_const_property_keeps_its_getter() {
    let source = r#"
var offset: Int = 0
val bufferSize: Int = 7
    get() = field + offset

fun box(): String {
    val first = bufferSize
    offset = 1
    val second = bufferSize
    return if (first == 7 && second == 8) "OK" else "FAIL"
}
"#;
    assert_accepted_and_runs(source, "NonConstPropertyGetter");
}

#[test]
fn star_imported_property_outprioritizes_a_default_imported_const_val() {
    const DECLARATION: &str = "package selected\nconst val DEFAULT_BUFFER_SIZE: Int = 7\n";
    const USE: &str = r#"
package use

import selected.*

fun box(): String = if (DEFAULT_BUFFER_SIZE == 7) "OK" else "FAIL"
"#;
    assert_accepted_and_runs_files(
        &[("Declaration.kt", DECLARATION), ("Use.kt", USE)],
        "ImportedConstValPrecedence",
    );
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
fn standalone_callable_reference_shape_mismatch_keeps_its_candidate_diagnostic() {
    let source = r#"
fun target(value: String): Int = value.length
val reference: (Int) -> Int = ::target
"#;
    let diagnostics = common::front_end_diagnostics_with_stdlib(source);
    assert_eq!(
        diagnostics,
        vec!["inapplicable candidate(s): fun target(value: String): Int"]
    );
}

#[test]
fn postponed_callable_reference_keeps_its_nested_unresolved_diagnostic() {
    let source = r#"
fun <T> consume(block: (T) -> T) {}
fun test() { consume(::missing) }
"#;
    let diagnostics = common::front_end_diagnostics_with_stdlib(source);
    assert_eq!(diagnostics, vec!["unresolved reference 'missing'."]);
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
        // The JDK supplies `java.io.Serializable`, a supertype of each of these builtins.
        Some(&common::jdk_modules()),
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
    assert_rejection_same_as_kotlinc("RepeatedContextResult", source);
}

#[test]
fn symbolic_and_concrete_contextual_result_constraints_conflict() {
    let source = r#"
interface Marker { fun mark(): String }
class Duo<A, B>

fun <T> build(transform: (T) -> String): Duo<T, T> = TODO()
fun <U : Marker> outer(): Duo<U, String> = build { it.mark() }
"#;
    assert_rejection_same_as_kotlinc("MixedContextResult", source);
}

#[test]
fn nested_contextual_nullable_result_keeps_the_lambda_input_nullable() {
    let source = r#"
interface Marker { fun mark(): String }

fun <T> build(transform: (T) -> String): List<T> = TODO()
fun <T : Marker> outer(): List<T?> = build { it.mark() }
"#;
    assert_rejection_same_as_kotlinc("NullableNestedContext", source);
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
fn nested_alias_constructors_inside_their_declaring_class_keep_target_identity() {
    let src = r#"
class Container {
    inner class Item(val value: String)
    inner class GenericItem<T>(val value: T)
    class NestedItem<T>(val value: T)

    typealias ItemAlias = Item
    typealias GenericAlias<T> = GenericItem<T>
    typealias NestedAlias = NestedItem<String>

    fun result(): String {
        if (ItemAlias("O").value != "O") return "item"
        if (GenericAlias("K").value != "K") return "generic"
        return NestedAlias("OK").value
    }
}

fun box(): String = Container().result()
"#;
    assert_accepted_and_runs(src, "ResolverNestedAliasConstructorIdentity");
}

#[test]
fn top_level_alias_to_an_inner_classifier_binds_the_implicit_outer_receiver() {
    let src = r#"
class Container {
    inner class Item(val value: String)
}
typealias ItemAlias = Container.Item

fun box(): String {
    val container = Container()
    val construct = Container::ItemAlias
    return construct(container, "OK").value
}
"#;
    assert_accepted_and_runs(src, "ResolverTopLevelInnerAliasImplicitReceiver");
}

#[test]
fn generic_top_level_alias_to_an_inner_classifier_maps_the_outer_argument() {
    let src = r#"
class Container<T>(val value: T) {
    inner class Item {
        fun result(): T = value
    }
}
typealias ItemAlias<T> = Container<T>.Item

fun box(): String {
    val container = Container("OK")
    if (container.ItemAlias<String>().result() != "OK") return "direct"
    val construct = Container<String>::ItemAlias
    return construct(container).result()
}
"#;
    assert_accepted_and_runs(src, "ResolverGenericTopLevelInnerAlias");
}

#[test]
fn inner_alias_outer_inference_substitutes_the_same_formal_in_own_arguments() {
    let src = r#"
class Outer<T>(val outer: T) {
    inner class Inner<U>(val inner: U) {
        fun result(): String = outer.toString() + inner.toString()
    }
}
typealias Same<T> = Outer<T>.Inner<T>

fun box(): String {
    val outer = Outer("O")
    val construct = Outer<String>::Same
    return construct(outer, "K").result()
}
"#;
    assert_accepted_and_runs(src, "ResolverInnerAliasSharedOuterFormal");
}

#[test]
fn fixed_inner_alias_outer_rejects_a_different_applied_receiver() {
    let src = r#"
class Container<T> {
    inner class Item
}
typealias StringItem = Container<String>.Item

fun byType() = Container<Int>::StringItem
"#;
    assert_rejection_same_as_kotlinc("ResolverInnerAliasFixedOuterMismatch", src);
}

#[test]
fn inapplicable_inner_alias_does_not_hide_an_applicable_extension() {
    let src = r#"
class Container<T> {
    inner class Item
}
typealias StringItem = Container<String>.Item

fun Container<Int>.StringItem(): String = "OK"

fun box(): String {
    if (Container<Int>().StringItem() != "OK") return "direct"
    val reference = Container<Int>::StringItem
    return reference(Container())
}
"#;
    assert_accepted_and_runs(src, "ResolverInnerAliasReceiverMismatchExtension");
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

#[test]
fn member_extension_null_literal_binds_receiver_bound_formal() {
    let source = r#"
class Box<out T : Any?> private constructor(private val value: Any?) {
    companion object {
        fun <T> Box<T>.getOrNull(): T? = getOrDefault(null)
        fun <T> Box<T>.getOrDefault(defaultValue: T): T = defaultValue
    }
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("MemberExtensionNullFormal", source);
    assert_eq!((code, diagnostics), (0, String::new()));
    let (diagnostics, selected) = common::inspect_checker_with_stdlib(source, |file, info, _| {
        file.expr_arena
            .iter()
            .enumerate()
            .filter(|(_, expression)| matches!(expression, krusty::ast::Expr::Call { .. }))
            .map(|(index, _)| krusty::ast::ExprId(index as u32))
            .map(|call| {
                let (owner, name, receiver, parameters) = info
                    .resolved_member_abi_signature(call)
                    .expect("selected member extension");
                (
                    owner,
                    name.to_string(),
                    receiver,
                    parameters.to_vec(),
                    info.resolved_call_is_member(call),
                    info.resolved_call_is_extension(call),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(diagnostics, Vec::<String>::new());
    let [(owner, name, Some(receiver), parameters, member, extension)] = selected.as_slice() else {
        panic!("expected one selected member extension, got {selected:#?}")
    };
    assert!(owner.matches("Box$Companion"));
    assert_eq!(name, "getOrDefault");
    assert!(*member);
    assert!(*extension);
    let [parameter] = parameters.as_slice() else {
        panic!("getOrDefault must retain one value parameter")
    };
    let krusty::types::TyKind::TyParam(parameter_identity, parameter_bound) = parameter.kind()
    else {
        panic!("generic value parameter was erased: {parameter:?}")
    };
    assert_eq!(
        parameter_bound,
        krusty::types::Ty::nullable(krusty::types::Ty::obj("kotlin/Any"))
    );
    assert!(matches!(
        receiver.kind(),
        krusty::types::TyKind::Obj(owner, arguments)
            if owner.matches("Box")
                && matches!(arguments.as_ref(), [argument]
                    if matches!(argument.kind(), krusty::types::TyKind::TyParam(receiver_identity, receiver_bound)
                        if receiver_identity == parameter_identity && receiver_bound == parameter_bound))
    ));
}

#[test]
fn member_extension_null_literal_still_rejected_for_invariant_receiver() {
    let source = r#"
class MutBox<T>(var v: T) {
    companion object {
        fun <T> MutBox<T>.getOrDefault(defaultValue: T): T = defaultValue
        fun read(box: MutBox<String>): String = box.getOrDefault(null)
    }
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("MemberExtensionInvariantNull", source);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [ObservedDiagnostic {
            file: "MemberExtensionInvariantNull.kt".to_string(),
            line: 5,
            column: 66,
            message: "null cannot be a value of a non-null type 'String'.".to_string(),
        }]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(source),
        ["null cannot be a value of a non-null type 'String'."]
    );
}

#[test]
fn inapplicable_receiver_extension_descends_to_explicit_import() {
    let src = r#"
import kotlin.math.min
fun CharSequence.f(endOffset: Int): Int = min(endOffset, 1)
fun box(): String = if ("abc".f(2) == 1) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ReceiverTowerImportDescent");
}

#[test]
fn inapplicable_receiver_member_descends_to_explicit_import() {
    let src = r#"
import kotlin.math.min
class C {
    fun min(): Int = 9
    fun f(endOffset: Int): Int = min(endOffset, 1)
}
fun box(): String = if (C().f(3) == 1) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ReceiverTowerMemberImportDescent");
}

#[test]
fn type_inapplicable_receiver_extension_descends_to_explicit_import() {
    let src = r#"
import kotlin.math.min
fun CharSequence.min(a: String, b: String): Int = a.length + b.length
fun CharSequence.f(endOffset: Int): Int = min(endOffset, 1)
fun box(): String = if ("abc".f(3) == 1) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ReceiverTowerTypeImportDescent");
}

#[test]
fn inapplicable_nearer_receiver_descends_to_outer_receiver() {
    let src = r#"
fun String.pick(): Int = 9
class Outer {
    fun pick(value: Int): Int = 100 + value
    fun result(): Int = with("") { pick(1) }
}
fun box(): String = if (Outer().result() == 101) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ReceiverTowerOuterReceiverDescent");
}

#[test]
fn applicable_receiver_extension_beats_explicit_import() {
    let src = r#"
import kotlin.math.min
fun CharSequence.min(a: Int, b: Int): Int = 1000 + a + b
fun CharSequence.f(endOffset: Int): Int = min(endOffset, 1)
fun box(): String = if ("abc".f(3) == 1004) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ReceiverTowerExtensionWins");
}

#[test]
fn applicable_receiver_member_beats_explicit_import() {
    let src = r#"
import kotlin.math.min
class C {
    fun min(a: Int, b: Int): Int = 500 + a + b
    fun f(endOffset: Int): Int = min(endOffset, 1)
}
fun box(): String = if (C().f(3) == 504) "OK" else "FAIL"
"#;
    assert_accepted_and_runs(src, "ReceiverTowerMemberWins");
}

#[test]
fn inapplicable_receiver_extension_without_import_keeps_arity_diagnostic() {
    let src = r#"
fun CharSequence.f(endOffset: Int): Int = min(endOffset, 1)
fun box(): String = "abc".f(2).toString()
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ReceiverTowerRetainedFailure", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [
            ObservedDiagnostic {
                file: "ReceiverTowerRetainedFailure.kt".to_string(),
                line: 2,
                column: 43,
                message: "return type mismatch: expected 'Int', actual 'Char'.".to_string(),
            },
            ObservedDiagnostic {
                file: "ReceiverTowerRetainedFailure.kt".to_string(),
                line: 2,
                column: 47,
                message: "too many arguments for 'fun CharSequence.min(): Char'.".to_string(),
            },
            ObservedDiagnostic {
                file: "ReceiverTowerRetainedFailure.kt".to_string(),
                line: 2,
                column: 58,
                message: "too many arguments for 'fun CharSequence.min(): Char'.".to_string(),
            },
        ]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [
            "too many arguments: expected at most 0".to_string(),
            "too many arguments: expected at most 0".to_string(),
        ]
    );
}

#[test]
fn inapplicable_receiver_member_without_later_candidate_keeps_arity_diagnostic() {
    let src = r#"
class C {
    fun min(): Int = 9
    fun f(endOffset: Int): Int = min(endOffset, 1)
}
"#;
    let (code, diagnostics) = common::kotlinc_source_result("ReceiverTowerMemberFailure", src);
    assert_eq!(code, 1);
    assert_eq!(
        reference_errors(&diagnostics),
        [
            ObservedDiagnostic {
                file: "ReceiverTowerMemberFailure.kt".to_string(),
                line: 4,
                column: 38,
                message: "too many arguments for 'fun min(): Int'.".to_string(),
            },
            ObservedDiagnostic {
                file: "ReceiverTowerMemberFailure.kt".to_string(),
                line: 4,
                column: 49,
                message: "too many arguments for 'fun min(): Int'.".to_string(),
            },
        ]
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(src),
        [
            "too many arguments: expected at most 0".to_string(),
            "too many arguments: expected at most 0".to_string(),
        ]
    );
}

#[test]
fn later_inapplicable_import_owns_the_terminal_diagnostic() {
    let sources = [
        (
            "Library.kt",
            r#"
package other
fun choose(value: String): Int = value.length
"#,
        ),
        (
            "Main.kt",
            r#"
import other.choose
fun CharSequence.choose(): Int = 0
fun CharSequence.f(): Int = choose(1)
"#,
        ),
    ];
    let result = common::compiler_diagnostics(&sources, &[common::stdlib_jar()]);
    assert_eq!(result.reference_code, 1);
    assert_eq!(
        reference_errors(&result.reference_stderr),
        [ObservedDiagnostic {
            file: "Main.kt".to_string(),
            line: 4,
            column: 36,
            message: "argument type mismatch: actual type is 'Int', but 'String' was expected."
                .to_string(),
        }]
    );
    let source_text = sources
        .iter()
        .map(|(_, source)| *source)
        .collect::<Vec<_>>();
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&source_text),
        ["argument type mismatch: actual type is 'Int', but 'String' was expected.".to_string()]
    );
}

#[test]
fn body_local_member_extension_val_rejects_write_like_kotlinc() {
    let source = r#"var Int.token: Int
    get() = 0
    set(value) {}

fun build() {
    class Local {
        val Int.token: Int get() = 1
        fun write() {
            1.token = 1
        }
    }
}
"#;
    assert_rejection_same_as_kotlinc("BodyLocalMemberExtensionVal", source);
}

/// A prefix with no value facet (a classifier with neither an object nor a companion, or a
/// package) that misses its next segment reports that segment, as kotlinc does. A root spelled
/// like both a default-imported classifier and the source package commits to the classifier, so
/// `Package.Outer` misses at `Outer` too.
#[test]
fn a_pure_qualifier_prefix_reports_its_missing_segment() {
    common::assert_errors_match_kotlinc(
        &[(
            "Qualified.kt",
            "package Package\n\
             class Outer { class Nested }\n\
             fun f(): Any = Thread.Missing.x\n\
             fun g(): Any = Thread.Missing.Nested()\n\
             fun h(): Any = Package.Outer.Nested()\n",
        )],
        &[],
    );
}

#[test]
fn qualified_nested_alias_constructors_continue_their_resolved_owner() {
    let src = r#"
package nested.aliases

class Outer {
    class Mid {
        class Leaf(val value: String)
        typealias LeafAlias = Leaf
    }
    class Box<T>(val value: T)
    typealias BoxAlias<T> = Box<T>
}

fun box(): String {
    val a = Outer.Mid.LeafAlias("O").value
    val b = nested.aliases.Outer.BoxAlias<String>("K").value
    return a + b
}
"#;
    assert_accepted_and_runs(src, "ResolverQualifiedNestedAliasConstructors");
}

/// A callable reference or class literal whose left-hand side names a typealias declared in a
/// classifier's body continues that classifier through the same qualifier step as an expression,
/// for a nested and for a local owner.
#[test]
fn nested_and_local_alias_references_continue_their_owner() {
    let src = r#"
class Item(val value: String)

class Outer {
    class Mid {
        class Leaf(val value: String)
        typealias LeafAlias = Leaf
    }
    typealias ItemAlias = Item
}

fun nested(): String {
    val a = Outer.Mid.LeafAlias::class.simpleName
    val b = Outer.ItemAlias::class.simpleName
    val c = Outer.Mid::LeafAlias
    val d = Outer::ItemAlias
    val e = Outer.Mid.LeafAlias::value
    return "$a $b ${c("C").value} ${d("D").value} ${e(Outer.Mid.Leaf("E"))}"
}

fun local(): String {
    class Local {
        typealias ItemAlias = Item
    }
    val a = Local.ItemAlias::class.simpleName
    val b = Local::ItemAlias
    val c = Local.ItemAlias::value
    return "$a ${b("B").value} ${c(Item("C"))}"
}

fun box(): String {
    val result = nested() + "|" + local()
    return if (result == "Leaf Item C D E|Item B C") "OK" else result
}
"#;
    common::expect_box_same_as_kotlinc_with_args(
        src,
        "ResolverNestedAndLocalAliasReferences",
        &["-XXLanguage:+LocalTypeAliases"],
    );
}

/// A typealias a local class declares selects the same declaration as a type, a constructor, a
/// runtime type operand and a callable-reference qualifier.
#[test]
fn a_local_class_alias_agrees_across_type_constructor_and_qualifier_uses() {
    let src = r#"
class Item(val value: String)

fun local(): String {
    class Local {
        typealias ItemAlias = Item
    }
    val typed: Local.ItemAlias = Local.ItemAlias("A")
    val any: Any = typed
    val checked = any is Local.ItemAlias
    val cast = (any as Local.ItemAlias).value
    val reference = Local.ItemAlias::value
    return "${typed.value} $checked $cast ${reference(Item("C"))}"
}

fun box(): String {
    val result = local()
    return if (result == "A true A C") "OK" else result
}
"#;
    common::expect_box_same_as_kotlinc_with_args(
        src,
        "ResolverLocalAliasUsesAgree",
        &["-XXLanguage:+LocalTypeAliases"],
    );
}

/// A qualified alias resolves its prefix left to right: a local class named `kotlin` binds the
/// root, so `kotlin.Exception` is that class's alias as a type and as a constructor, never the
/// same-spelled `kotlin.Exception` typealias of the standard library.
#[test]
fn a_local_owner_named_like_a_package_selects_its_own_alias() {
    let src = r#"
class Num(val value: Int)

fun local(): String {
    class kotlin {
        typealias Exception = Num
    }
    val typed: kotlin.Exception = kotlin.Exception(7)
    return "${typed.value}"
}

fun box(): String {
    val result = local()
    return if (result == "7") "OK" else result
}
"#;
    common::expect_box_same_as_kotlinc_with_args(
        src,
        "ResolverLocalOwnerShadowsPackageAlias",
        &["-XXLanguage:+LocalTypeAliases"],
    );
}

#[test]
fn a_package_qualified_property_value_is_invoked() {
    let src = r#"
package invoked.values

val greeting = { "O" }
object Suffix { operator fun invoke() = "K" }
val suffix = Suffix

fun box(): String = invoked.values.greeting() + invoked.values.suffix()
"#;
    assert_accepted_and_runs(src, "ResolverPackageQualifiedPropertyInvoke");
}

#[test]
fn a_callable_import_keeps_the_default_imported_classifier_in_type_positions() {
    let sources = [
        (
            "lib.kt",
            r#"package lib

fun Pair(value: Int): kotlin.Pair<Int, Int> = kotlin.Pair(value, value + 1)
"#,
        ),
        (
            "main.kt",
            r#"import lib.Pair

fun make(value: Int): Pair<Int, Int>? = Pair(value)

fun Pair<Int, Int>.sum(): Int = first + second

fun box(): String {
    val pair = make(1) ?: return "null"
    return if (pair.sum() == 3) "OK" else "FAIL: $pair"
}
"#,
        ),
    ];
    assert_accepted_and_runs_files(&sources, "ResolverCallableImportDefaultClassifier");
    let differences = common::classes_against_kotlinc_module(&sources).differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn an_implicit_receiver_extension_property_root_binds_a_callable_suffix() {
    // The root `kotlin` names both a root package and an extension property on the lambda's
    // implicit receiver; the property wins at that receiver's level and the callable suffix binds
    // on its value.
    let src = r#"
class Holder(val text: String)

val Holder.kotlin: String get() = text

fun viaLambdaReceiver(holder: Holder): String = with(holder) { kotlin.uppercase() }

fun box(): String = viaLambdaReceiver(Holder("ok"))
"#;
    let stem = "ResolverImplicitReceiverPropertyCallSuffix";
    assert_accepted_and_runs(src, stem);
    common::assert_classes_identical_to_kotlinc(
        stem,
        src,
        &["ResolverImplicitReceiverPropertyCallSuffixKt", "Holder"],
    );
}

/// An extension function's type parameter is named with its owner's receiver as the declaration
/// spells it (`fun <T> T?.orName`), as kotlinc words it; a function without a receiver keeps the
/// bare name.
#[test]
fn an_extension_function_type_parameter_owner_names_its_receiver() {
    let sources = [(
        "Owner.kt",
        r#"fun <T : Number> List<T>.firstName(): T = "first"
fun <T> T?.orName(): T = "name"
fun <K, V> Map<in K, V>.valueName(key: K): V? = key.hashCode()
fun <T : CharSequence> plainName(): T = 1
"#,
    )];
    let reference_args = common::language_directives::kotlinc_args(sources[0].1);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Owner.kt:1:43: return type mismatch: expected 'T (of fun <T : Number> List<T>.firstName)', actual 'String'.",
            "Owner.kt:2:26: return type mismatch: expected 'T (of fun <T> T?.orName)', actual 'String'.",
            "Owner.kt:3:49: return type mismatch: expected 'V? (of fun <K, V> Map<in K, V>.valueName)', actual 'Int'.",
            "Owner.kt:4:41: return type mismatch: expected 'T (of fun <T : CharSequence> plainName)', actual 'Int'.",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

/// Only a formal constrained from above meets its declared bound. `S` of `put` also occurs in an
/// ordinary value parameter here, so its binding is exact and `T`'s mismatch is kotlinc's error.
#[test]
fn an_in_projected_formal_with_an_ordinary_occurrence_keeps_its_exact_binding() {
    let sources = [(
        "Sink.kt",
        r#"class Sink<T>
fun <T, S : T?> Sink<in S>.put(value: T, extra: S): Int = 1

fun <T : Number?, S : T> both(sink: Sink<T>, t: S, n: Int): Int = sink.put(t, n)
"#,
    )];
    let reference_args = common::language_directives::kotlinc_args(sources[0].1);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Sink.kt:4:79: argument type mismatch: actual type is 'Int', but 'T (of fun <T : Number?, S : T> both)' was expected.",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

/// The builder shape from a SQL library: a member extension's defaulted `separator` rejects the
/// builder that a top-level extension of the same name accepts, and a trailing receiver lambda
/// follows. kotlinc selects the top-level extension; the classes match its output.
#[test]
fn a_rejected_member_extension_gives_way_to_a_top_level_builder_extension() {
    let src = r#"
class Builder {
    var out = ""

    fun <T> List<T>.appendTo(separator: String = ", ", prefix: String = "", transform: Builder.(T) -> Unit) {
        out += prefix
        for (index in indices) {
            if (index > 0) out += separator
            transform(this[index])
        }
    }
}

fun <T> List<T>.appendTo(builder: Builder, separator: String = ", ", prefix: String = "", transform: Builder.(T) -> Unit): Builder {
    with(builder) { this@appendTo.appendTo(separator, prefix, transform) }
    return builder
}

fun render(items: List<String>): String = with(Builder()) {
    items.appendTo(this, prefix = "SET ") { out += it }
    out
}

fun box(): String {
    val rendered = render(listOf("a", "b"))
    return if (rendered == "SET a, b") "OK" else "FAIL: $rendered"
}
"#;
    let stem = "ResolverMemberExtensionYieldsToBuilderExtension";
    assert_accepted_and_runs(src, stem);
    common::assert_classes_identical_to_kotlinc(
        stem,
        src,
        &[
            "ResolverMemberExtensionYieldsToBuilderExtensionKt",
            "Builder",
        ],
    );
}

/// A member extension that rejects the arguments gives way only to an applicable top-level
/// extension. When the top-level extension does not accept the receiver, the member extension's
/// own argument mismatch is the diagnostic, as kotlinc reports it.
#[test]
fn a_rejected_member_extension_without_an_applicable_top_level_extension_reports_its_mismatch() {
    let sources = [(
        "Render.kt",
        r#"class Builder {
    fun <T> Iterable<T>.appendTo(separator: CharSequence, transform: Builder.(T) -> Unit) {}
}

fun <T> Set<T>.appendTo(builder: Builder, transform: Builder.(T) -> Unit) {}

fun render(items: List<String>): Unit = with(Builder()) {
    items.appendTo(this) { }
}
"#,
    )];
    let reference_args = common::language_directives::kotlinc_args(sources[0].1);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        [
            "Render.kt:8:20: argument type mismatch: actual type is 'Builder', but 'CharSequence' was expected.",
        ]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn a_bound_violating_only_argument_leaves_the_formal_uninferred() {
    // kotlinc rejects the argument's constraint at the argument; with no other evidence left, it
    // first reports that `T` cannot be inferred.
    let source = r#"
open class Shape
class Square : Shape()
class Keep<T : Square>(item: T)
fun <T : Square> keep(item: T): Int = 1
fun shape(): Shape = Shape()
fun call(): Int = keep(shape())
fun construct(): Keep<*> = Keep(shape())
"#;
    assert_rejection_same_as_kotlinc("BoundViolatingOnlyArgument", source);
}

#[test]
fn a_bound_violation_after_valid_evidence_blames_the_later_argument() {
    let source = r#"
open class Shape
class Square : Shape()
class KeepTwo<T : Square>(first: T, second: T)
fun <T : Square> keepTwo(first: T, second: T): Int = 1
fun shape(): Shape = Shape()
fun call(): Int = keepTwo(Square(), shape())
fun construct(): KeepTwo<*> = KeepTwo(Square(), shape())
"#;
    assert_rejection_same_as_kotlinc("BoundViolationAfterValidEvidence", source);
}

#[test]
fn an_invariant_argument_fixes_the_formal_before_a_later_argument() {
    // `MutableList<Square>` fixes `T` to `Square`, so the `Shape` argument is the contradiction;
    // joining both inputs to `Shape` would blame the list instead.
    let source = r#"
open class Shape
class Square : Shape()
class Same<T>(items: MutableList<T>, item: T)
fun <T> same(items: MutableList<T>, item: T): Int = items.size
fun shape(): Shape = Shape()
fun call(): Int = same(mutableListOf<Square>(), shape())
fun construct(): Same<*> = Same(mutableListOf<Square>(), shape())
"#;
    assert_rejection_same_as_kotlinc("InvariantArgumentFixesFormal", source);
}

#[test]
fn a_mixed_variance_parameter_does_not_fix_its_formal() {
    // `(T) -> T` mentions `T` in both directions, which is not an equality constraint: `T` solves
    // to `Any` from the function's `Any` input bound together with the `Int` argument.
    let src = r#"
class Apply<T>(val f: (T) -> T, val value: T)
fun <T> apply(f: (T) -> T, value: T): T = f(value)
fun box(): String {
    val f: (Any) -> String = { "OK" }
    val constructed = Apply(f, 1)
    return if (apply(f, 1) == "OK" && constructed.f(constructed.value) == "OK") "OK" else "FAIL"
}
"#;
    assert_accepted_and_runs(src, "MixedVarianceParameter");
}

#[test]
fn a_bound_and_an_invariant_contradiction_reject_every_argument() {
    // The list's `Circle` leaves `T : Square`, so it fixes nothing; `shape` is then checked
    // against the bound alone and rejected too. With no evidence left, `T` is uninferred and the
    // list parameter renders it so.
    let source = r#"
open class Shape
class Square : Shape()
class Circle : Shape()
class Keep<T : Square>(items: MutableList<T>, item: T)
fun <T : Square> keep(items: MutableList<T>, item: T) {}
fun call(shape: Shape) {
    keep(mutableListOf<Circle>(), shape)
}
fun construct(shape: Shape): Keep<*> = Keep(mutableListOf<Circle>(), shape)
"#;
    assert_rejection_same_as_kotlinc("BoundAndInvariantContradictions", source);
}

#[test]
fn a_rejected_structural_argument_renders_its_uninferred_formal() {
    let source = r#"
open class Shape
class Square : Shape()
class KeepAll<T : Square>(items: List<T>)
fun <T : Square> keepAll(items: List<T>): Int = 1
fun shape(): Shape = Shape()
fun call(): Int = keepAll(listOf(shape()))
fun construct(): KeepAll<*> = KeepAll(listOf(shape()))
"#;
    assert_rejection_same_as_kotlinc("RejectedStructuralArgument", source);
}

#[test]
fn one_structural_argument_rejects_each_formal_it_contradicts() {
    // kotlinc reports every formal the one argument leaves uninferred, then the argument once.
    let source = r#"
class Duo<A, B>
fun <A : Number, B : CharSequence> take(value: Duo<A, B>) {}
class Sink<A : Number, B : CharSequence>(value: Duo<A, B>)
fun test(value: Duo<String, Int>) {
    take(value)
    Sink(value)
}
"#;
    assert_rejection_same_as_kotlinc("StructuralArgumentRejectsEachFormal", source);
}
