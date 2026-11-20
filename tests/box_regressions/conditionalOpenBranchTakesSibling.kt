// MODULE: lib
// A dependency call that leaves a result-only type parameter open (`none()`) completes it to
// its bound on its own. As one branch of a conditional whose type is inferred, it takes that
// argument from the sibling branch, as kotlinc completes the conditional as one call: the
// conditional keeps the sibling's invariant type instead of a projected join. A sibling that is
// a subtype binds through its supertype face (`Named<V> : Entry<String, V>`).
open class Entry<K, V>(val key: K?, val value: V?)

class Named<V>(key: String, value: V) : Entry<String, V>(key, value)

fun <K, V> none(): Entry<K, V> = Entry(null, null)

fun <K, V> entry(key: K, value: V): Entry<K, V> = Entry(key, value)

fun <V> named(value: V): Named<V> = Named("O", value)

fun <K, V> keyOf(entry: Entry<K, V>): K? = entry.key

fun <K, V> valueOf(entry: Entry<K, V>): V? = entry.value

// MODULE: main(lib)
fun first(c: Boolean) = if (c) none() else entry("O", "K")

fun last(c: Boolean) = if (c) entry("O", 1) else none()

fun nested(c: Boolean) = if (c) null else when { c -> none() else -> entry("O", "K") }

fun faced(c: Boolean) = if (c) none() else named("K")

fun box(): String {
    val picked = first(false)
    val key: String = keyOf(picked) ?: return "first key"
    val value: String = valueOf(picked) ?: return "first value"
    if (key != "O" || value != "K") return "first"
    val counted: Int = valueOf(last(true)) ?: return "last"
    if (counted != 1) return "last"
    val inner = nested(false) ?: return "nested"
    val innerKey: String = keyOf(inner) ?: return "nested key"
    val innerValue: String = valueOf(inner) ?: return "nested value"
    if (innerKey + innerValue != "OK") return "nested"
    val facedKey: String = keyOf(faced(false)) ?: return "faced key"
    val facedValue: String = valueOf(faced(false)) ?: return "faced value"
    return facedKey + facedValue
}
