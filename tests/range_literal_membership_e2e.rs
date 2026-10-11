//! Membership in a range literal is compiled like kotlinc's `RangeContainsLowering`: `x in a..b`
//! is `a <= x && x <= b` in the widest numeric type of the three, with a temporary only for an
//! operand that may have side effects, and a constant inclusive upper bound made exclusive. A
//! plain value read keeps kotlinc's comparison order: a failed lower comparison may skip an
//! effectful upper bound, and a mutable local is read before the upper expression. A `downTo`
//! progression and a user `contains` extension are called.

use super::common;

const FUNCTIONS: &str = r#"
var log = ""

fun next(value: Int): Int {
    log += value
    return value
}

fun nextLong(value: Long): Long {
    log += value
    return value
}

operator fun IntRange.contains(value: Short): Boolean = value.toInt() == 7
operator fun IntRange.contains(value: Byte): Boolean = value.toInt() == 1

fun sameType(x: Int) = x in 1..10
fun sameTypeLong(x: Long) = x in 1L..10L
fun maxBound(x: Int) = x in 1..Int.MAX_VALUE
fun until(x: Int) = x in 1 until 10
fun openEnd(x: Int, end: Int) = x in 1..<end
fun character(c: Char) = c in 'a'..'z'
fun floating(x: Double) = x in 1.0..2.0
fun notIn(x: Int) = x !in 1..10
fun notInFloating(x: Double) = x !in 1.0..2.0
fun longValue(x: Long) = x in 1..10
fun byteValue(x: Byte) = x in 1..10
fun byteBounds(x: Int, a: Byte, b: Byte) = x in a..b
fun floatValue(x: Float) = x in 1.0..2.0
fun longBounds(x: Int) = x in 1L..nextLong(10L)
fun constantValue() = 5 in next(1)..10
fun upperEffect(x: Int) = x in 1..next(10)
fun mutableValueReadPrecedesUpper(initial: Int): Boolean {
    var value = initial
    return value in 1..when {
        else -> {
            value = 5
            3
        }
    }
}
fun lowerEffect(x: Int) = x in next(1)..10
fun bothEffects(x: Int) = x in next(1)..next(10)
fun valueEffect() = next(5) in next(1)..next(10)
fun valueEffectConstantBounds() = next(5) in 1..10
fun downTo(x: Int) = x in 10 downTo 1
fun userExtension(x: Short) = x in 1..10
fun uniformByte(value: Byte, start: Byte, end: Byte) = value in start..end
fun uniformShort(value: Short, start: Short, end: Short) = value in start..end
"#;

const BOX: &str = r#"fun box(): String {
    val results = listOf(
        sameType(10), sameTypeLong(11L), maxBound(Int.MAX_VALUE), until(10), openEnd(4, 5),
        character('q'), floating(2.0), notIn(0), notInFloating(Double.NaN), longValue(10L),
        byteValue(1), byteBounds(3, 2, 4), floatValue(1.5f), longBounds(10), constantValue(),
        upperEffect(3), upperEffect(0), mutableValueReadPrecedesUpper(2), lowerEffect(3),
        bothEffects(3), valueEffect(), valueEffectConstantBounds(), downTo(10), userExtension(7),
        userExtension(5), uniformByte(2, 1, 3), uniformShort(2, 1, 3),
    )
    return if (results == listOf(
            true, false, true, false, true, true, true, true, true, true, true, true, true, true,
            true, true, false, true, true, true, true, true, true, true, false, false, false,
        ) && log == "10110111011055") "OK" else "$results $log"
}
"#;

#[test]
fn range_literal_membership_is_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "RangeLiteralMembership",
        FUNCTIONS,
        &["RangeLiteralMembershipKt"],
    );
}

#[test]
fn range_literal_membership_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(&format!("{FUNCTIONS}{BOX}"), "range_literal_membership");
}

#[test]
fn platform_floating_nan_not_in_negates_the_positive_membership() {
    let java = [(
        "PlatformValue.java".to_string(),
        "public final class PlatformValue {\
            public static Double nan() { return Double.NaN; }\
            public static Float nanFloat() { return Float.NaN; }\
        }"
        .to_string(),
    )];
    let (directory, _) = common::javac_compile(&java, &[]).expect("compile Java platform value");
    let source = r#"
fun platformDoubleNotIn(): Boolean = PlatformValue.nan() !in 1.0..2.0
fun platformFloatNotIn(): Boolean = PlatformValue.nanFloat() !in 1.0f..2.0f
fun box(): String = if (platformDoubleNotIn() && platformFloatNotIn()) "OK" else "FAIL"
"#;
    common::assert_class_code_matches_kotlinc_jdk_cp(
        "PlatformFloatingRange",
        source,
        "PlatformFloatingRangeKt",
        std::slice::from_ref(&directory),
    );
    common::expect_true_e2e(
        "platform Float and Double NaN !in floating ranges",
        source,
        std::slice::from_ref(&directory),
    );
    if let Some(root) = directory.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
}
