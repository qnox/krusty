// MODULE: lib
// A library's public inline functions are inlined at their call sites, as kotlinc does, so a
// lambda passed to one may return non-locally from its caller, with a value or with Unit.
class Reader(private val text: String) {
    fun readAll(): String = text
}

inline fun <T> read(path: String, action: Reader.() -> T): T = Reader(path).action()

inline fun <T> twice(action: () -> T): T {
    action()
    return action()
}

inline fun <T : CharSequence> measured(value: T, measure: (T) -> Int): Int = measure(value)

// MODULE: main(lib)
fun firstRead(): String {
    read("O") {
        return readAll() + "K"
    }
}

fun stopEarly(log: StringBuilder) {
    twice {
        log.append("before")
        return
    }
}

fun box(): String {
    val value = firstRead()
    if (value != "OK") return "value: $value"
    val log = StringBuilder()
    stopEarly(log)
    if (log.toString() != "before") return "unit: $log"
    var count = 0
    val result = twice { ++count }
    if (result != 2) return "twice: $result"
    if (measured("four") { it.length } != 4) return "measured"
    return "OK"
}
