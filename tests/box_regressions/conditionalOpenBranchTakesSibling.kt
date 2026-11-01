// MODULE: lib
// A dependency call whose own arguments leave its type arguments open completes them to `Any`; as
// one branch of a conditional it takes them from the sibling branch, so the conditional keeps the
// sibling's invariant type instead of a projected join.
class Entry<K, V>(val key: K?, val value: V?)

fun <K, V> none(): Entry<K, V> = Entry(null, null)

fun <K, V> entry(key: K, value: V): Entry<K, V> = Entry(key, value)

fun <K, V> keyOf(entry: Entry<K, V>): K? = entry.key

fun <K, V> valueOf(entry: Entry<K, V>): V? = entry.value

// MODULE: main(lib)
fun first(c: Boolean) = if (c) none() else entry("O", "K")

fun last(c: Boolean) = if (c) entry("O", 1) else none()

fun nested(c: Boolean) = if (c) null else when { c -> none() else -> entry("O", "K") }

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
    return innerKey + innerValue
}
