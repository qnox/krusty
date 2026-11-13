// An integer literal adapts to a nullable expected type as it does to the non-null one: `Short?`
// takes `1`, `UShort?` takes `65535u`, whether the expectation reaches the literal through a
// parameter or through the element type a generic call is solved for, whether the result position
// is invariant (`Array<T>`) or covariant (`Box<out T>`) and whether another argument is `null`.
class Box<out T>(val first: T, val second: T)

fun <T> boxOf(first: T, second: T): Box<T> = Box(first, second)

fun toLong(x: Long?): Long = x ?: -1L

fun box(): String {
    if (toLong(3) != 3L) return "parameter"
    val shorts: Array<Short?> = arrayOf(1, null)
    if (shorts[0] != 1.toShort() || shorts[1] != null) return "shorts"
    val longs: Array<Long?> = arrayOf(2, null)
    if (longs[0] != 2L || longs[1] != null) return "longs"
    val unsigned: Array<UShort?> = arrayOf(65535u, null)
    if (unsigned[0]!!.toInt() != 65535 || unsigned[1] != null) return "unsigned"
    val covariant: Box<Short?> = boxOf(1, null)
    if (covariant.first != 1.toShort() || covariant.second != null) return "covariant"
    val reversed: Box<Long?> = boxOf(null, 3)
    if (reversed.first != null || reversed.second != 3L) return "reversed"
    val unsignedBox: Box<UShort?> = boxOf(65535u, null)
    if (unsignedBox.first!!.toInt() != 65535 || unsignedBox.second != null) return "unsigned covariant"
    return "OK"
}
