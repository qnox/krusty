// A sized array starts with its element's default: `arrayOfNulls` and `Array(n) { }` over
// reference storage, and the primitive size constructors over their own. A negative size throws
// before any initializer runs.
fun box(): String {
    val nulls = arrayOfNulls<String>(2)
    if (nulls.size != 2 || nulls[0] != null || nulls[1] != null) return "nulls"
    nulls[1] = "K"
    val squares = Array(3) { it * it }
    if (squares.size != 3 || squares[2] != 4) return "initialized"
    val ints = IntArray(2)
    if (ints.size != 2 || ints[0] != 0 || ints[1] != 0) return "ints"
    val booleans = BooleanArray(1)
    if (booleans[0]) return "booleans"
    val chars = CharArray(3) { 'a' + it }
    if (chars[2] != 'c') return "chars"
    if (arrayOfNulls<Any>(0).size != 0) return "empty"
    try {
        IntArray(-1)
        return "negative"
    } catch (e: Throwable) {
        if (e !is RuntimeException) return "negative kind"
    }
    try {
        arrayOfNulls<Any>(-1)
        return "negative nulls"
    } catch (e: Throwable) {
        if (e !is RuntimeException) return "negative nulls kind"
    }
    var initialized = 0
    try {
        Array(-1) { initialized++ }
        return "negative initialized"
    } catch (e: Throwable) {
        if (e !is RuntimeException) return "negative initialized kind"
    }
    if (initialized != 0) return "initializer ran"
    return "O" + nulls[1]
}
