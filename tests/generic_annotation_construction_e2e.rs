//! PLATFORM: JVM annotation attributes and Kotlin metadata retain checked generic construction.

use super::common;

const POSITIVE: &str = r#"annotation class Leaf<T>(val n: Int)
annotation class Nest<T>(val leaves: Array<Leaf<T>>)
annotation class Envelope(val leaf: Leaf<String>)
annotation class ArrayEnvelope(val nested: Nest<String>)
annotation class GenericEnvelope<T>(val leaf: Leaf<T>)
annotation class Fixed(val leaves: Array<Leaf<String>>)
annotation class FixedEnvelope(val nested: Fixed)
@Envelope(Leaf(1)) class Inferred
@Envelope(Leaf<String>(2)) class Explicit
@ArrayEnvelope(Nest<String>([Leaf<String>(1), Leaf<String>(2)])) class ArrayValues
@ArrayEnvelope(Nest<String>([])) class EmptyValues
@GenericEnvelope<String>(Leaf(3)) class GenericInferred
@GenericEnvelope<String>(Leaf<String>(4)) class GenericExplicit
@FixedEnvelope(Fixed([Leaf(5), Leaf<String>(6)])) class FixedContext
fun box(): String = "OK"
"#;

const NEGATIVE: &str = r#"annotation class Leaf<T>(val n: Int)
annotation class Bounded<T : CharSequence>(val n: Int)
annotation class Nest<T>(val leaves: Array<Leaf<T>>)
annotation class Envelope(val leaf: Leaf<String>)
annotation class BoundEnvelope(val leaf: Bounded<String>)
annotation class ArrayEnvelope(val nested: Nest<String>)
annotation class GenericEnvelope<T>(val leaf: Leaf<T>)
@Envelope(Leaf<Int>(1)) class WrongExplicit
@BoundEnvelope(Bounded<Int>(1)) class WrongBound
@ArrayEnvelope(Nest<String>([Leaf<Int>(2)])) class WrongArrayElement
@GenericEnvelope<String>(Leaf<Int>(3)) class WrongGenericOwner
fun make(): Leaf<String> = Leaf(1)
@Envelope(make()) class NonConstant
"#;

const DIRECT_ARRAY: &str = r#"annotation class Leaf<T>(val n: Int)
annotation class Direct(val leaves: Array<Leaf<String>>)
@Direct([Leaf(1), Leaf<String>(2)]) class Mixed
@Direct([Leaf(1)]) class Single
"#;

const INFERRED_ARRAY_REJECTED: &str = r#"annotation class Leaf<T>(val n: Int)
annotation class Nest<T>(val leaves: Array<Leaf<T>>)
annotation class Envelope(val nested: Nest<String>)
@Envelope(Nest<String>([Leaf(1), Leaf<String>(2)])) class Rejected
"#;

#[test]
fn nested_generic_annotations_match_the_complete_reference_class_inventory() {
    let classes = common::classes_against_kotlinc_module(&[("Main.kt", POSITIVE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn contradictory_arguments_bounds_and_nonconstant_calls_match_every_reference_diagnostic() {
    common::assert_errors_match_kotlinc(&[("Main.kt", NEGATIVE)], &[]);
}

#[test]
fn direct_annotation_array_slots_keep_their_declared_element_context() {
    let classes = common::classes_against_kotlinc_module(&[("Direct.kt", DIRECT_ARRAY)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn constructor_array_inference_is_not_replaced_by_the_enclosing_result_expectation() {
    common::assert_errors_match_kotlinc(&[("Main.kt", INFERRED_ARRAY_REJECTED)], &[]);
}

#[test]
fn dependency_annotation_constructors_keep_their_applied_results() {
    let library = "package dependency\nannotation class Leaf<T>(val n: Int)\nannotation class Envelope(val leaf: Leaf<String>)\n";
    let source = "import dependency.Leaf as Node\nimport dependency.Envelope\n@Envelope(Node(1)) class Inferred\n@Envelope(Node<String>(2)) class Explicit\n";
    let classes = common::classes_against_kotlinc_lib(
        "DependencyAnnotations",
        &[("Annotations.kt", library)],
        source,
    )
    .expect("reference compiler provisions the annotation dependency");
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn qualified_annotation_constructors_keep_primitive_and_empty_array_context() {
    let source = "class Host {\n\
                  annotation class Numbers(val xs: IntArray, val bs: ByteArray)\n\
                  }\n\
                  annotation class Envelope(val nested: Host.Numbers)\n\
                  @Envelope(Host.Numbers([1], [3])) class Values\n\
                  @Envelope(Host.Numbers([], [])) class Empty\n";
    let classes = common::classes_against_kotlinc_module(&[("Qualified.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
    let classes = common::classes_against_kotlinc_lib(
        "QualifiedDependency",
        &[("Declarations.kt", "package dependency\nclass Host { annotation class Numbers(val xs: IntArray, val bs: ByteArray) }\nannotation class Envelope(val nested: Host.Numbers)\n")],
        "import dependency.Host\nimport dependency.Envelope\n@Envelope(Host.Numbers([1], [3])) class Values\n@Envelope(Host.Numbers([], [])) class Empty\n",
    ).expect("reference compiler provisions the qualified annotation dependency");
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn an_imported_alias_with_the_same_target_does_not_replace_a_nested_classifier() {
    let sources = [
        ("Alias.kt", "package other\ntypealias A<T> = host.Host.A<Int, T>\n"),
        ("Host.kt", "package host\nimport other.A\nclass Host {\nannotation class Leaf<T>(val n: Int)\nannotation class A<X, Y>(val leaf: Leaf<Y>)\n@A<Long, String>(Leaf<String>(1)) fun tagged() {}\n}\n"),
    ];
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_selected_dependency_alias_keeps_its_template_beside_an_unrelated_source_alias() {
    let library = common::kotlinc_lib_out(&[(
        "Library.kt",
        "package dependency\nannotation class Leaf<T>(val n: Int)\nannotation class Pair<A, B>(val leaf: Leaf<B>)\ntypealias Alias<T> = Pair<Int, T>\n",
    )]).expect("reference compiler provisions the alias dependency");
    let sources = [
        ("Unrelated.kt", "package unrelated\ntypealias Alias<T> = dependency.Pair<T, String>\n"),
        ("Use.kt", "package application\nimport dependency.Alias\nimport dependency.Leaf\n@Alias<Long>(Leaf<Long>(1)) class Tagged\n"),
    ];
    let classes = common::classes_against_kotlinc_module_cp(&sources, &[library]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}
