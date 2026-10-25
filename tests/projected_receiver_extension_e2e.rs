//! Reading a member off a USE-SITE PROJECTED type. Everything read out of `List<out Range>` is a
//! `Range`, so `first()` returns a `Range` and its members resolve. krusty let the generic-argument
//! projection escape as the call expression's top-level type, so `c[0].startOffset` resolved (the
//! indexed-member path specializes output position) while `c.first().startOffset` did not.
//!
//! Inferring a type parameter FROM a projected receiver is a separate gap and is not fixed here:
//! `c.map { it.startOffset }` still fails, because `T` does not bind from `List<out Range>` and the
//! lambda body is then checked against an inapplicable candidate. See `docs/SPEC.md`.

use super::common;

fn run(source: &str) -> Option<String> {
    common::compile_and_run_box(
        source,
        "Main",
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    )
}

#[test]
fn a_member_reads_off_a_projected_extension_result() {
    let source = r#"
        class Range(val startOffset: Int, val endOffset: Int)

        fun firstStart(ranges: List<out Range>): Int = ranges.first().startOffset

        fun box(): String = "${firstStart(listOf(Range(7, 9)))}"
    "#;

    assert_eq!(run(source).as_deref(), Some("7"));
}

#[test]
fn a_projected_element_reads_the_same_as_an_indexed_one() {
    let source = r#"
        class Range(val startOffset: Int, val endOffset: Int)

        fun ends(ranges: List<out Range>): String =
            "${ranges[0].endOffset}${ranges.last().endOffset}"

        fun box(): String = ends(listOf(Range(1, 2), Range(3, 4)))
    "#;

    assert_eq!(run(source).as_deref(), Some("24"));
}

#[test]
fn an_invariant_receiver_still_reads_its_members() {
    let source = r#"
        class Range(val startOffset: Int, val endOffset: Int)

        fun starts(ranges: List<Range>): List<Int> = ranges.map { it.startOffset }

        fun box(): String = starts(listOf(Range(5, 6))).joinToString(",")
    "#;

    assert_eq!(run(source).as_deref(), Some("5"));
}
#[test]
fn a_contravariant_receiver_infers_an_anonymous_generic_extension_result() {
    let source = r#"
interface Invariant<A>
fun Invariant<in Number>.publicFunc() = privateFunc()
private fun <B> Invariant<B>.privateFunc() = object : Invariant<B> {}
fun box() = "OK"
"#;
    super::common::expect_box_same_as_kotlinc(
        source,
        "contravariant_receiver_anonymous_generic_result",
    );
}

const CAPTURED_RECEIVERS: &str = r#"
interface Token<A>
private fun <T> Token<T>.copy() = object : Token<T> {}
private fun <T> Token<T>.echo(value: T) = value
fun invariant(value: Token<Number>) = value.copy()
fun contravariant(value: Token<in Number>) = value.copy()
fun covariant(value: Token<out Number>) = value.copy()
fun star(value: Token<*>) = value.copy()
fun writable(value: Token<in Number>): Any? = value.echo(7)
fun box(): String {
    val numbers = object : Token<Number> {}
    val anything = object : Token<Any?> {}
    invariant(numbers)
    contravariant(anything)
    covariant(numbers)
    star(anything)
    return if (writable(anything) == 7) "OK" else "value"
}
"#;

#[test]
fn receiver_captures_preserve_invariant_covariant_contravariant_and_star_results() {
    common::expect_box_same_as_kotlinc(CAPTURED_RECEIVERS, "projected_receiver_capture_results");
}

// PLATFORM: complete JVM member signatures for the generic result and receiver applications.
#[test]
fn inferred_receiver_capture_signatures_match_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[(
        "ProjectedReceiverCaptures.kt",
        CAPTURED_RECEIVERS,
    )]);
    let name = "ProjectedReceiverCapturesKt";
    assert_eq!(
        common::member_table(classes.krusty.get(name).expect("compiled facade")),
        common::member_table(classes.reference.get(name).expect("reference facade")),
    );
}

const ANONYMOUS_RESULT_EXPOSURE: &str = r#"
interface Token<T> { fun value(): T }
interface Marker
open class Base<T>(val stored: T)
private fun <T> copy(value: T) = object : Token<T> {
    override fun value(): T = value
    val extra: T = value
}
private inline fun inlineCopy(value: Int) = object : Token<Int> {
    override fun value(): Int = value
}
private fun noParentCopy() = object { val extra = "OK" }
private fun multipleCopy() = object : Token<Int>, Marker {
    override fun value(): Int = 7
}
private fun baseCopy() = object : Base<Int>(7), Marker {}
fun exposed() = copy(7)
fun exposedInline() = inlineCopy(7)
fun noParent() = noParentCopy()
fun multipleParents(): Any = multipleCopy()
fun baseParent(): Base<Int> = baseCopy()
fun direct() = object : Token<Int> { override fun value(): Int = 7 }
fun localMember(): String {
    class Local { fun create() = object { val extra = "OK" } }
    return Local().create().extra
}
fun box(): String = if (copy(7).extra == 7 && exposed().value() == 7 &&
    exposedInline().value() == 7 && direct().value() == 7 && localMember() == "OK")
    "OK" else "value"
"#;

// PLATFORM: complete JVM facade signatures distinguish exact and exposed anonymous results.
#[test]
fn anonymous_factory_exposure_policy_signatures_match_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[(
        "AnonymousResultExposure.kt",
        ANONYMOUS_RESULT_EXPOSURE,
    )]);
    let name = "AnonymousResultExposureKt";
    assert_eq!(
        common::member_table(classes.krusty.get(name).expect("compiled facade")),
        common::member_table(classes.reference.get(name).expect("reference facade")),
    );
}

#[test]
fn anonymous_factory_exposure_policy_preserves_private_and_local_members() {
    common::expect_box_same_as_kotlinc(
        ANONYMOUS_RESULT_EXPOSURE,
        "anonymous_result_exposure_policy",
    );
}

const ANONYMOUS_RESULT_BOUNDS: &str = r#"
interface Token<T>
interface Bounded<T : Number>
class Box<T>(val item: T)
private fun <T> Token<T>.copy() = object : Token<T> {}
private fun <T : Number> Bounded<T>.copy() = object : Bounded<T> {}
private fun functionCopy() = object : () -> String {
    override fun invoke(): String = "OK"
}
fun bounded(value: Bounded<*>) = value.copy()
fun writtenStar(value: Token<*>) = value
fun nested(value: Token<Number>) = listOf(value.copy())
fun invariantNested(value: Token<Number>) = Box(value.copy())
fun callback(value: Token<Number>) = { value.copy() }
fun functionParent() = functionCopy()
fun box(): String {
    val value = object : Token<Number> {}
    val boundedValue = object : Bounded<Int> {}
    bounded(boundedValue)
    writtenStar(value)
    callback(value)()
    invariantNested(value).item
    return if (nested(value).size == 1 && functionParent()() == "OK") "OK" else "size"
}
"#;

// PLATFORM: complete JVM signatures preserve written stars and approximate captured bounds.
#[test]
fn anonymous_factory_bounds_and_nested_results_match_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[(
        "AnonymousResultBounds.kt",
        ANONYMOUS_RESULT_BOUNDS,
    )]);
    let name = "AnonymousResultBoundsKt";
    assert_eq!(
        common::member_table(classes.krusty.get(name).expect("compiled facade")),
        common::member_table(classes.reference.get(name).expect("reference facade")),
    );
}

#[test]
fn anonymous_factory_bounds_and_nested_results_run_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        ANONYMOUS_RESULT_BOUNDS,
        "anonymous_result_bounds_and_nested_results",
    );
}

#[test]
fn anonymous_factory_multiple_parent_results_require_explicit_types_like_kotlinc() {
    let source = r#"
interface First
interface Second
open class Base
private fun interfaces() = object : First, Second {}
private fun baseAndInterface() = object : Base(), First {}
fun multipleInterfaces() = interfaces()
fun multipleParents() = baseAndInterface()
"#;
    let result = common::compiler_diagnostics(
        &[("Main.kt", source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&result, "inferred anonymous results with multiple parents");
}

fn assert_complete_abi_and_metadata(classes: &common::ClassSets) {
    let inventory = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| {
                let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
                let header = format!(
                    "{:?}",
                    (
                        info.access,
                        info.this_class,
                        info.super_class,
                        info.interfaces(),
                        info.signature,
                        info.inner_classes,
                    )
                );
                (
                    name.clone(),
                    (
                        header,
                        common::member_table(bytes),
                        (
                            format!("{:?}", info.meta),
                            common::kotlin_metadata::kotlin_metadata_elements(bytes),
                        ),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let actual = inventory(&classes.krusty);
    let expected = inventory(&classes.reference);
    let actual_abi = actual
        .iter()
        .map(|(name, (header, members, _))| (name.clone(), (header.clone(), members.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    let expected_abi = expected
        .iter()
        .map(|(name, (header, members, _))| (name.clone(), (header.clone(), members.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual_abi, expected_abi, "complete class ABI");
    assert_eq!(actual, expected, "complete class metadata");
}

// PLATFORM: every generated class's complete header, member ABI and Kotlin metadata annotation.
#[test]
fn anonymous_factory_exposure_preserves_complete_class_abi_and_metadata() {
    for (file, source) in [
        ("AnonymousResultExposure.kt", ANONYMOUS_RESULT_EXPOSURE),
        ("AnonymousResultBounds.kt", ANONYMOUS_RESULT_BOUNDS),
    ] {
        assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
            file, source,
        )]));
    }
}

const ANONYMOUS_RESULT_VARIANCE: &str = r#"
interface Token<T>
class Sink<in T>
class Box<T>
interface Holder<T>
interface Parent<T>
private fun <T> copy(value: T) = object : Token<T> {}
private fun <T> consumer(value: T): (T) -> String = { "OK" }
private fun <T> boxedConsumer(value: T): (Box<T>) -> String = { "OK" }
private fun <T> twice(value: T): ((T) -> String) -> String = { it(value) }
private fun <T> sink(value: T) = Sink<T>()
private fun <T> Holder<T>.nestedCopy() = object : Parent<MutableList<in T>> {}
fun input() = consumer(copy(7))
fun boxedInput() = boxedConsumer(copy(7))
fun twiceInput() = twice(copy(7))
fun contravariantContainer() = sink(copy(7))
fun invariantParent(value: Holder<Number>) = value.nestedCopy()
fun starParent(value: Holder<*>) = value.nestedCopy()
fun outParent(value: Holder<out Number>) = value.nestedCopy()
fun inParent(value: Holder<in Number>) = value.nestedCopy()
fun box(): String = (twiceInput()) { "OK" }
"#;

// PLATFORM: complete result and implementation ABI across negative and nested type positions.
#[test]
fn anonymous_factory_negative_and_nested_parent_positions_match_kotlinc() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "AnonymousResultVariance.kt",
        ANONYMOUS_RESULT_VARIANCE,
    )]));
}

#[test]
fn anonymous_factory_negative_and_nested_parent_positions_run_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        ANONYMOUS_RESULT_VARIANCE,
        "anonymous_result_negative_and_nested_parent_positions",
    );
}

const ANONYMOUS_RESULT_GETTERS: &str = r#"
interface Token<T>
private fun copy() = object : Token<Number> {}
val callback get() = { copy() }
class GetterOwner {
    val callback get() = { copy() }
    fun callback() = { copy() }
}
fun box(): String {
    callback()
    GetterOwner().callback()
    GetterOwner().callback()()
    return "OK"
}
"#;

// PLATFORM: inferred getters expose their declaration result while private lambda methods stay exact.
#[test]
fn anonymous_factory_inferred_getters_preserve_complete_class_abi_and_metadata() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "AnonymousResultGetters.kt",
        ANONYMOUS_RESULT_GETTERS,
    )]));
}

#[test]
fn anonymous_factory_inferred_getters_run_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        ANONYMOUS_RESULT_GETTERS,
        "anonymous_result_inferred_getters",
    );
}

#[test]
fn anonymous_factory_multiple_parent_function_input_requires_explicit_type_like_kotlinc() {
    let source = r#"
interface First
interface Second
private fun copy() = object : First, Second {}
private fun <T> consumer(value: T): (T) -> Unit = {}
fun input() = consumer(copy())
fun box(): String = "OK"
"#;
    let result = common::compiler_diagnostics(
        &[("Main.kt", source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&result, "anonymous multiple-parent function input");
}

const ANONYMOUS_RESULT_NESTED_CAPTURES: &str = r#"
interface Token<T>
class Carrier<T>(val value: T)
private fun <T> innerCopy(value: T) = object : Token<T> {}
private fun <T> Carrier<T>.outView(): Carrier<out T> = this
private fun <T> Carrier<T>.inView(): Carrier<in T> = this
private fun <T> Carrier<T>.copy() = object : Token<T> {}
fun outResult() = Carrier(innerCopy(7)).outView().copy()
fun inResult() = Carrier(innerCopy(7)).inView().copy()
fun box(): String {
    outResult()
    inResult()
    return "OK"
}
"#;

// PLATFORM: full class ABI and metadata for anonymous payloads inside captured projections.
#[test]
fn anonymous_factory_nested_projected_payloads_match_kotlinc() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "AnonymousResultNestedCaptures.kt",
        ANONYMOUS_RESULT_NESTED_CAPTURES,
    )]));
}

#[test]
fn anonymous_factory_nested_projected_payloads_run_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        ANONYMOUS_RESULT_NESTED_CAPTURES,
        "anonymous_result_nested_projected_payloads",
    );
}
