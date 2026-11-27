// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: `@JvmStatic` exists only on the JVM; a sibling file's reads and writes of an
// object's or a companion's property must call the static accessors kotlinc declares.
// FILE: Declarations.kt
object Ide {
    /** 16x16 */ @JvmStatic val A: String = "O"
    @JvmStatic var V: Int = 1
    @JvmStatic val C: String get() = "c"
    @JvmStatic lateinit var L: String
    @JvmStatic @get:JvmName("named") val N: Int = 3
    fun readBack(): String = A + V + C + N
}
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

// FILE: Main.kt
fun box(): String {
    Ide.V = 5
    Ide.L = "l"
    H.W = 6
    H.L = "m"
    return if (Ide.A + Ide.V + Ide.C + Ide.L + H.B + H.W + H.C + H.L == "O5clK6cm") "OK" else "fail"
}
