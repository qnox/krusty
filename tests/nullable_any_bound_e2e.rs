//! `R : Any` accepts a type parameter when every use of `R` is nullable.
//!
//! `firstNotNullOfOrNull` is `fun <T, R : Any> Iterable<T>.firstNotNullOfOrNull(transform: (T) -> R?): R?`.
//! The lambda result binds `R` to the caller's type parameter. A caller bounded by `Any?` is not a
//! subtype of non-null `Any`, but the nullable use contributes `T & Any` and the result reopens as
//! `T?`. A caller already bounded by `Any` keeps that bound.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn a_nullable_any_bound_accepts_the_caller_type_parameter() {
    expect_box_same_as_kotlinc(
        r#"
fun <T, R : Any> Iterable<T>.pick(transform: (T) -> R?): R? {
    for (item in this) {
        val value = transform(item)
        if (value != null) return value
    }
    return null
}

fun <R : Any> take(value: R?): R? = value

fun <R : Any> nullable(block: () -> R?): R? = block()

fun <R : Any> combine(nullable: R?, exact: R): R? = nullable

class Payload(val value: Int)

fun <T> combined(value: T): T? = combine(value, value!!)

fun <K, V> fromMap(maps: List<Map<K, V>>, key: K): V? = maps.pick { it[key] }

fun <K, V> inferred(maps: List<Map<K, V>>, key: K): V? =
    maps.firstNotNullOfOrNull { it[key] }

fun <K, V> openInferred(maps: List<Map<K, V>>, key: K): V? {
    val found = maps.firstNotNullOfOrNull { it[key] }
    return found
}

fun <T> taken(value: T): T? = take(value)

fun <T : CharSequence?> takenBound(value: T): T? = take(value)

fun <T : Any> takenNonNull(value: T): T? = take(value)

fun <T> fromLambda(value: T): T? = nullable { value }

fun <T : Any> kept(items: Iterable<T?>): List<T> = items.filterNotNull()

fun box(): String {
    val maps = listOf(mapOf("a" to 1), mapOf("b" to 2))
    if (fromMap(maps, "b") != 2) return "pick"
    if (inferred(maps, "a") != 1) return "stdlib"
    if (openInferred(maps, "b") != 2) return "open"
    if (taken("x") != "x") return "take"
    val text: CharSequence? = "a"
    if (takenBound(text) != "a") return "bound"
    if (takenNonNull("y") != "y") return "nonNull"
    if (fromLambda(7) != 7) return "lambda"
    if (kept(listOf("p", null)) != listOf("p")) return "filter"
    if (combined(Payload(9))?.value != 9) return "combine"
    return "OK"
}
"#,
        "NullableAnyBound",
    );
}
