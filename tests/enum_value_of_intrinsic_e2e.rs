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
