//! Metadata baselines whose source does not need exposed anonymous result publication.

// PLATFORM: complete JVM facade metadata payloads against the reference compiler.
use super::common;

fn assert_facade_metadata(stem: &str, source: &str) {
    let file = format!("{stem}.kt");
    let classes = common::classes_against_kotlinc_module(&[(&file, source)]);
    let name = format!("{stem}Kt");
    let metadata = |bytes: &Vec<u8>| {
        let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
        (
            format!("{:?}", info.meta),
            common::kotlin_metadata::kotlin_metadata_ints(bytes),
            common::kotlin_metadata::kotlin_metadata_d1_parts(bytes),
            common::raw_kotlin_metadata(bytes),
        )
    };
    assert_eq!(metadata(&classes.krusty[&name]), metadata(&classes.reference[&name]));
}

#[test]
fn private_generic_anonymous_factory_metadata_matches_kotlinc() {
    assert_facade_metadata("GenericAnonymousMetadataBaseline", r#"
interface Token<T>
private fun <T> copy(value: T) = object : Token<T> {}
fun box(): String = "OK"
"#);
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

#[test]
fn anonymous_factory_negative_and_nested_parent_positions_match_kotlinc() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "AnonymousResultVariance.kt",
        ANONYMOUS_RESULT_VARIANCE,
    )]));
}

#[test]
fn anonymous_factory_nested_projected_payloads_match_kotlinc() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "AnonymousResultNestedCaptures.kt",
        ANONYMOUS_RESULT_NESTED_CAPTURES,
    )]));
}

#[test]
fn anonymous_factory_bounds_preserve_complete_class_abi_and_metadata() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "AnonymousResultBounds.kt",
        ANONYMOUS_RESULT_BOUNDS,
    )]));
}

#[test]
fn a_named_local_class_keeps_its_own_formal_and_captured_method_formal() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "NamedLocalMetadata.kt",
        r#"
private fun <T> build(value: T): Any {
    class Local<U>(val own: U) { val captured: T = value }
    return Local(7)
}
fun box(): String { build("OK"); return "OK" }
"#,
    )]));
}

#[test]
fn a_named_local_formal_can_shadow_its_enclosing_method_formal() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "ShadowedLocalMetadata.kt",
        r#"
private fun <T> build(value: T): Any {
    class Local<T>(val own: T)
    return Local(value)
}
fun box(): String { build("OK"); return "OK" }
"#,
    )]));
}

#[test]
fn an_ordinary_inner_instance_keeps_its_outer_type_arguments() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "InnerApplicationMetadata.kt",
        r#"
class Outer<T>(val captured: T) { inner class Inner<U>(val own: U) }
fun make() = Outer("O").Inner("K")
fun box(): String { val value = make(); return value.own }
"#,
    )]));
}

