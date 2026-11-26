// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: `@JvmStatic` exists only on the JVM, where kotlinc makes the outer class
// declare a static bridge for each accessor of a companion property.
class H {
    companion object {
        @JvmStatic val B: String = "K"
        @JvmStatic var W: Int = 1
        @JvmStatic val C: String get() = "c"
        @JvmStatic lateinit var L: String
        @JvmStatic @get:JvmName("named") val N: Int = 3
        fun readBack(): String = B + W + C + N
    }
}

fun box(): String {
    H.W = 2
    H.L = "l"
    return if (H.B + H.W + H.C + H.L + H.N + H.readBack() == "K2cl3K2c3") "OK" else "fail"
}
