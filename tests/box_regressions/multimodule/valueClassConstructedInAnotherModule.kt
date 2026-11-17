// A module constructs a value class its library declares. The library's constructor runs, `init`
// block included, and the instance is a member call's receiver and result.
// MODULE: lib
// FILE: Lib.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class Meters(val value: Int) {
    init {
        require(value >= 0) { "negative: $value" }
    }

    fun twice() = Meters(value * 2)
}

// MODULE: main(lib)
// FILE: Main.kt
fun box(): String {
    val m = Meters(3)
    if (m.twice().value != 6) return "twice = ${m.twice().value}"
    val rejected = try {
        Meters(-1)
        null
    } catch (e: IllegalArgumentException) {
        e.message
    }
    return if (rejected == "negative: -1") "OK" else "rejected = $rejected"
}
