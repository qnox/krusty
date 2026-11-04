//! Kotlin annotation INSTANTIATION (Kotlin 1.6+): calling an `annotation class`'s constructor (`A(5)`)
//! constructs an annotation instance whose members are readable (`a.x`) and whose `equals`/`hashCode`/
//! `toString` follow the `java.lang.annotation.Annotation` contract (content equality, `Arrays`-aware,
//! `Float`/`Double` NaN/`-0.0` semantics). krusty emits the annotation as a JVM annotation interface plus
//! a synthetic impl class implementing that contract. Verified by running `box()` on a real JVM.

use super::common;

const EXPLICIT_RECEIVER: &str = r#"annotation class Explicit(val value: Int)
fun box(): String = if (Explicit(7).value == 7) "OK" else "bad"
"#;

#[test]
fn annotation_instantiation_explicit_receiver_matches_complete_kotlin_artifacts() {
    assert_complete_artifacts("ExplicitAnnotationReceiver.kt", EXPLICIT_RECEIVER);
}

fn assert_complete_artifacts(file: &str, source: &str) {
    let classes = common::classes_against_kotlinc_module(&[(file, source)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>(),
        "the complete emitted class inventory must match"
    );
    let differences = classes
        .reference
        .iter()
        .filter_map(|(name, reference)| {
            let ours = &classes.krusty[name];
            (reference != ours).then(|| common::exact_class_difference(name, reference, ours))
        })
        .collect::<Vec<_>>();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn annotation_instantiation_explicit_receiver_runs_like_kotlin() {
    common::expect_box_same_as_kotlinc(EXPLICIT_RECEIVER, "ExplicitAnnotationReceiver");
}

const CONSUMPTION_CONTEXTS: &str = r#"annotation class Explicit(val value: Int)
class Holder(val item: Explicit = Explicit(7))
fun declaredResult(): Explicit = Explicit(7)
fun erasedResult(): Any = Explicit(7)
fun argument(value: Explicit): Int = value.value
fun box(): String {
    val inferred = Explicit(7)
    val declared: Explicit = Explicit(7)
    val array = arrayOf(Explicit(7))
    if (inferred.value != 7 || declared.value != 7) return "locals"
    if (declaredResult().value != 7 || argument(Explicit(7)) != 7) return "calls"
    if ((erasedResult() as Explicit).value != 7) return "erased"
    if (array[0].value != 7 || Holder().item.value != 7) return "slots"
    return "OK"
}
"#;

#[test]
fn annotation_instantiation_consumers_match_complete_kotlin_artifacts() {
    assert_complete_artifacts("AnnotationConsumerSlots.kt", CONSUMPTION_CONTEXTS);
}

#[test]
fn annotation_instantiation_consumers_run_like_kotlin() {
    common::expect_box_same_as_kotlinc(CONSUMPTION_CONTEXTS, "AnnotationConsumerSlots");
}

const ORDINARY_RECEIVER: &str = r#"open class Parent { val value: Int = 7 }
class Child : Parent()
fun box(): String = if (Child().value == 7) "OK" else "bad"
"#;

#[test]
fn annotation_instantiation_leaves_ordinary_receivers_byte_identical_to_kotlin() {
    assert_complete_artifacts("OrdinaryReceiver.kt", ORDINARY_RECEIVER);
}

#[test]
fn annotation_instantiation_ordinary_receiver_runs_like_kotlin() {
    common::expect_box_same_as_kotlinc(ORDINARY_RECEIVER, "OrdinaryReceiver");
}

fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "T")
}

#[test]
fn annotation_instantiation_member_read_and_content_equality() {
    let src = "annotation class A(val x: Int, val s: String)\n\
        fun box(): String {\n\
        \x20 val a = A(5, \"hi\"); val b = A(5, \"hi\"); val c = A(6, \"hi\")\n\
        \x20 if (a.x != 5 || a.s != \"hi\") return \"read\"\n\
        \x20 if (a != b) return \"eq\"\n\
        \x20 if (a.hashCode() != b.hashCode()) return \"hash\"\n\
        \x20 if (a == c) return \"neq\"\n\
        \x20 return \"OK\"\n}\n";
    let output = run(src);
    assert_eq!(output.trim(), "OK", "box() = {output:?}");
}

#[test]
fn annotation_instantiation_array_member_content_equality() {
    let src = "annotation class A(val xs: IntArray)\n\
        fun box(): String {\n\
        \x20 val a: Any = A(intArrayOf(1, 2, 3)); val b: Any = A(intArrayOf(1, 2, 3)); val c: Any = A(intArrayOf(1, 3, 2))\n\
        \x20 if (!a.equals(b)) return \"eq\"\n\
        \x20 if (a.hashCode() != b.hashCode()) return \"hash\"\n\
        \x20 if (a.equals(c)) return \"neq\"\n\
        \x20 return \"OK\"\n}\n";
    let output = run(src);
    assert_eq!(output.trim(), "OK", "box() = {output:?}");
}

#[test]
fn annotation_instantiation_nested_and_tostring() {
    let src = "annotation class Inner(val v: String)\n\
        annotation class Outer(val inn: Inner, val n: Int)\n\
        fun box(): String {\n\
        \x20 val a = Outer(Inner(\"q\"), 7); val b = Outer(Inner(\"q\"), 7); val c = Outer(Inner(\"z\"), 7)\n\
        \x20 if (a != b) return \"eq\"\n\
        \x20 if (a == c) return \"neq\"\n\
        \x20 if (a.inn.v != \"q\") return \"read\"\n\
        \x20 if (!a.toString().contains(\"Outer(\")) return \"toString:\" + a.toString()\n\
        \x20 return \"OK\"\n}\n";
    let output = run(src);
    assert_eq!(output.trim(), "OK", "box() = {output:?}");
}
