//! `value class` without `@JvmInline`.
//!
//! `+FullValueClasses` makes it a boxed class with structural `equals`/`hashCode`/`toString`.
//! Without the feature the declaration is rejected. `@JvmInline` (including an import alias)
//! keeps the unboxed inline ABI.

use super::common;

#[test]
fn value_class_without_jvm_inline_is_rejected_until_the_feature_is_enabled() {
    const SRC: &str = "value class A(val x: Int)\nfun box() = \"OK\"\n";
    assert_eq!(
        common::front_end_diagnostics(SRC, &[], None),
        vec!["value classes without '@JvmInline' annotation are not yet supported.".to_string()]
    );
}

#[test]
fn full_value_class_matches_data_class_equality() {
    const SRC: &str = "\
// LANGUAGE: +FullValueClasses
value class A1(val x: Int)
value class A2(val x: Int, val y: Int)
data class A1_(val x: Int)
data class A2_(val x: Int, val y: Int)
class Outer {
    value class Inner(val x: Int, val y: Int)
}
fun box(): String {
    val a = A1(2)
    val a_ = A1_(2)
    if (a != A1(2)) return \"eq1\"
    if (a.x != a_.x) return \"x\"
    if (a.toString() != a_.toString().replace(\"_\", \"\")) return \"str \" + a.toString()
    if (a.hashCode() != a_.hashCode()) return \"hash\"
    val b = A2(2, 3)
    val b_ = A2_(2, 3)
    if (b != A2(2, 3)) return \"eq2\"
    if (b.x != b_.x || b.y != b_.y) return \"xy\"
    if (b.toString() != b_.toString().replace(\"_\", \"\")) return \"str2 \" + b.toString()
    if (b.hashCode() != b_.hashCode()) return \"hash2\"
    val inner = Outer.Inner(1, 2)
    if (inner != Outer.Inner(1, 2) || inner.toString() != \"Inner(x=1, y=2)\") return inner.toString()
    return \"OK\"
}
";
    common::expect_box_ok_with_stdlib(SRC, "full_value_class_matches_data_class_equality");
}

#[test]
fn jvm_inline_value_class_stays_unboxed() {
    const SRC: &str = "\
@JvmInline
value class Id(val x: Int)
fun box(): String {
    val text = Id(7).toString()
    return if (text == \"7\") \"OK\" else text
}
";
    common::expect_box_ok_with_stdlib(SRC, "jvm_inline_value_class_stays_unboxed");
}

#[test]
fn aliased_jvm_inline_annotation_stays_unboxed() {
    const SRC: &str = "\
import kotlin.jvm.JvmInline as Inline
@Inline
value class Id(val x: Int)
fun box(): String {
    val text = Id(7).toString()
    return if (text == \"7\") \"OK\" else text
}
";
    common::expect_box_ok_with_stdlib(SRC, "aliased_jvm_inline_annotation_stays_unboxed");
}
