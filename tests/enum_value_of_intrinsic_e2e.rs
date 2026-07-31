//! `enumValueOf<E>(name)` / `enumValues<E>()` — reified enum-reflection intrinsics declared in the
//! kotlin builtins with no JVM facade. kotlinc emits the enum's synthetic statics (`E.valueOf(name)` /
//! `E.values()`); the synthetic registry realizes the same IR, the reified `E` taken from the call's
//! type argument (including a reified type parameter forwarded through an `inline` function).
use super::common;

#[test]
fn enum_value_of_and_values_intrinsics_run() {
    let src = "enum class Color { RED, GREEN, BLUE }\n\
fun box(): String {\n\
val c = enumValueOf<Color>(\"GREEN\")\n\
val all = enumValues<Color>()\n\
if (c != Color.GREEN) return \"f1\"\n\
if (all.size != 3 || all[0] != Color.RED || all[2] != Color.BLUE) return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "EnumValueOf");
}

#[test]
fn enum_value_of_forwarded_through_reified_inline() {
    // A reified type parameter forwarded to `enumValueOf<T>` inside an `inline` function, called with an
    // explicit type argument — the registry resolves `T` via `reified_subst` at the expanded call site.
    let src = "enum class Color { RED, GREEN }\n\
inline fun <reified T : Enum<T>> parse(s: String): T = enumValueOf<T>(s)\n\
fun box(): String = if (parse<Color>(\"GREEN\") == Color.GREEN) \"OK\" else \"no\"\n";
    common::expect_box_ok_with_stdlib(src, "EnumValueOfForward");
}

#[test]
fn enum_value_of_rejects_a_non_enum_type() {
    let src = "fun box(): String = enumValueOf<String>(\"OK\")\n";
    assert!(
        common::compile_and_run_with_stdlib(src, "EnumValueOfNonEnum").is_none(),
        "a non-enum type argument must not reach enum intrinsic lowering"
    );
}

#[test]
fn enum_value_of_safe_wrapper_through_reified_inline() {
    // The `safeEnumValueOf` idiom: a reified `T : Enum<T>` forwarded to `enumValueOf<T>` inside a
    // `try`/`catch` EXPRESSION. The try's value merges `T` (checked against the erased `Enum` bound)
    // with `null` to `T?`, and the expansion's result slot is typed by that erased bound — the value
    // is cast back to the call-site `Color?` at the expansion boundary.
    let src = "enum class Color { RED, GREEN, BLUE }\n\
private inline fun <reified T : Enum<T>> safeEnumValueOf(value: String?): T? {\n\
    if (value == null) return null\n\
    return try {\n\
        enumValueOf<T>(value)\n\
    } catch (_: IllegalArgumentException) {\n\
        null\n\
    }\n\
}\n\
fun box(): String {\n\
    val c = safeEnumValueOf<Color>(\"RED\")\n\
    val bad = safeEnumValueOf<Color>(\"NOPE\")\n\
    val nul = safeEnumValueOf<Color>(null)\n\
    return if (c == Color.RED && bad == null && nul == null) \"OK\" else \"FAIL\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "EnumValueOfSafeWrapper");
}

#[test]
fn enum_value_of_reified_inline_expression_body_try() {
    // Expression-bodied variant — no `return` statement, so the expansion yields the try value
    // directly; the erased-view → call-site cast happens at the expansion boundary too.
    let src = "enum class Color { RED, GREEN }\n\
inline fun <reified T : Enum<T>> parseOrNull(s: String): T? =\n\
    try { enumValueOf<T>(s) } catch (e: IllegalArgumentException) { null }\n\
fun box(): String =\n\
    if (parseOrNull<Color>(\"GREEN\") == Color.GREEN && parseOrNull<Color>(\"zz\") == null) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "EnumValueOfExprBodyTry");
}

#[test]
fn unrelated_reified_parameter_does_not_erase_primitive_return_slot() {
    // Merely declaring a reified parameter must not erase a separate generic return. `U` is
    // specialized to `Int` by the existing inline-call path, so both the parameter and return slots
    // remain primitive; treating every return from a reified function as erased would instead mix an
    // `Int` body value with an `Object` result slot and fail JVM frame verification.
    let src = "inline fun <reified T, U> keep(value: U): U = value\n\
fun box(): String = if (keep<String, Int>(42) == 42) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "UnrelatedReifiedPrimitiveReturn");
}

#[test]
fn unrelated_member_reified_parameter_does_not_erase_primitive_return_slot() {
    // Member-call resolution already carries the call-site-specialized `U` return (`Int`). Keep the
    // regression separate from the top-level path so the reified-return guard cannot accidentally
    // depend on which source-call lookup supplied the signature.
    let src = "class Host {\n\
    inline fun <reified T, U> keep(value: U): U = value\n\
}\n\
fun box(): String = if (Host().keep<String, Int>(42) == 42) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "UnrelatedMemberReifiedPrimitiveReturn");
}
