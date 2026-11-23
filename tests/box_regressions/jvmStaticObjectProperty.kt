// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: `@JvmStatic` exists only on the JVM, where kotlinc makes a named object's
// property accessors and backing field statics of the object.
object Ide {
    /** 16x16 */ @JvmStatic val A: String = "O"
    @JvmStatic var V: Int = 1
    @JvmStatic val C: String get() = "c"
    @JvmStatic lateinit var L: String
    @JvmStatic @get:JvmName("named") val N: Int = 3
    fun readBack(): String = A + V + C + N
}

fun box(): String {
    Ide.V = 2
    Ide.L = "l"
    return if (Ide.A + Ide.V + Ide.C + Ide.L + Ide.N + Ide.readBack() == "O2cl3O2c3") "OK" else "fail"
}
