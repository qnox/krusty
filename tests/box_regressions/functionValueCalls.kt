// A lambda or a callable reference is a value: stored, passed and called through its function
// type, its captures travelling with it and its primitives boxed across the call (kotlinc 2.4.21).
fun one(): Int = 1

fun twice(x: Long): Long = x * 2

class Counter(var total: Int) {
    fun add(step: Int): Int {
        total += step
        return total
    }
}

fun apply(f: (Int, Int) -> Int, a: Int, b: Int): Int = f(a, b)

fun <T, R> map(value: T, f: (T) -> R): R = f(value)

var effects = ""

fun box(): String {
    val base = 40
    val add: (Int) -> Int = { it + base }
    if (add(2) != 42) return "capture"
    if (apply({ a, b -> a * b }, 6, 7) != 42) return "passed"
    val read: () -> Int = ::one
    if (read() != 1) return "reference"
    val double: (Long) -> Long = ::twice
    if (double(21L) != 42L) return "long reference"
    val counter = Counter(40)
    val step: (Int) -> Int = counter::add
    if (step(1) != 41 || step(1) != 42 || counter.total != 42) return "bound reference"
    if (map(20) { it * 2 + 2 } != 42) return "generic"
    if (map("ab") { it + "c" } != "abc") return "string"
    if (map(1.5) { it > 1.0 } != true) return "boolean result"
    val record: (String) -> Unit = { effects += it }
    record("o")
    record("k")
    if (effects != "ok") return "unit"
    val nothing: () -> Unit = {}
    nothing()
    return "OK"
}
