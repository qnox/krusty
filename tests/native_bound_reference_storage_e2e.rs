//! PLATFORM: Native callable-reference receiver storage, equality and thunk conversion ABI.

use crate::common;

fn check(source: &str, stem: &str) {
    assert_eq!(common::kotlinc_box_result(source), "OK");
    common::expect_native_box(source, stem, "OK");
}

#[test]
fn scalar_bound_receivers_keep_object_equality_and_typed_invocation() {
    check(
        r#"
private var evaluations = 0
private fun receiver(): Int { evaluations++; return 1000 }
private fun Int.boundAdd(value: Int): Int = this + value
private fun Long.boundAdd(value: Long): Long = this + value
fun box(): String {
    val first = receiver()::boundAdd
    val same = 1000::boundAdd
    val different = 1001::boundAdd
    if (evaluations != 1) return "receiver evaluation"
    if (first != same || first.hashCode() != same.hashCode()) return "Int equality/hash"
    if (first == different) return "different Int receiver"
    if (first(2) != 1002 || first(3) != 1003 || evaluations != 1) return "Int invocation"
    val wide = 10000000000L::boundAdd
    val wideSame = 10000000000L::boundAdd
    if (wide != wideSame || wide.hashCode() != wideSame.hashCode()) return "Long equality/hash"
    return if (wide(2L) == 10000000002L) "OK" else "Long invocation"
}
"#,
        "NativeBoundScalarStorage",
    );
}

#[test]
fn value_class_bound_receivers_keep_semantic_boxes_for_equality() {
    check(
        r#"
@JvmInline value class Count(val value: Int)
@JvmInline value class Label(val value: String)
private fun Any?.tag(): Int = 7
private fun Count.add(value: Int): Int = this.value + value
private fun Label.append(value: String): String = this.value + value
fun box(): String {
    val count = Count(1000)
    val boxedCount: Any = count
    val firstCount = count::tag
    val secondCount = boxedCount::tag
    if (firstCount != secondCount || firstCount.hashCode() != secondCount.hashCode())
        return "scalar value-class equality/hash"
    val label = Label("O")
    val boxedLabel: Any = label
    val firstLabel = label::tag
    val secondLabel = boxedLabel::tag
    if (firstLabel != secondLabel || firstLabel.hashCode() != secondLabel.hashCode())
        return "reference value-class equality/hash"
    if (firstCount() != 7 || firstLabel() != 7) return "erased invocation"
    val typedCount = count::add
    val sameCount = Count(1000)::add
    val differentCount = Count(1001)::add
    if (typedCount != sameCount || typedCount.hashCode() != sameCount.hashCode())
        return "typed scalar value-class equality/hash"
    if (typedCount == differentCount) return "different scalar value-class receiver"
    val typedLabel = label::append
    val sameLabel = Label("O")::append
    val differentLabel = Label("other")::append
    if (typedLabel != sameLabel || typedLabel.hashCode() != sameLabel.hashCode())
        return "typed reference value-class equality/hash"
    if (typedLabel == differentLabel) return "different reference value-class receiver"
    if (typedCount(2) != 1002 || typedLabel("K") != "OK") return "typed invocation"
    return "OK"
}
"#,
        "NativeBoundValueClassStorage",
    );
}

#[test]
fn a_bound_receiver_stays_after_ordinary_scalar_captures() {
    check(
        r#"
private fun make(extra: Int): (Int) -> Int {
    fun Int.localAdd(value: Int): Int = this + extra + value
    return 1000::localAdd
}
fun box(): String {
    val reference = make(7)
    reference.hashCode()
    return if (reference(2) == 1009 && reference(3) == 1010) "OK" else "capture order"
}
"#,
        "NativeBoundMixedCaptureStorage",
    );
}
