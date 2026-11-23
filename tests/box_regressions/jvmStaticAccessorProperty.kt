// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: `@JvmStatic` exists only on the JVM, where kotlinc makes only the annotated
// accessor of a property static, or bridged.
object O {
    @get:JvmStatic val r: Int = 7
    @set:JvmStatic var s: Int = 8
    @JvmStatic var w: String = "a"
        @JvmStatic get() = field + "!"
    @JvmStatic var u: String = "a"
        get() = field + "!"
        set(value) { field = value + "?" }
}

class H {
    companion object {
        @get:JvmStatic val r: Int = 7
        @set:JvmStatic var s: Int = 8
        @JvmStatic var w: String = "a"
            @JvmStatic get() = field + "!"
        @JvmStatic var u: String = "a"
            get() = field + "!"
            set(value) { field = value + "?" }
    }
}

fun box(): String {
    O.s = 9
    O.u = "x"
    H.s = 9
    H.u = "x"
    val objectValue = "" + O.r + O.s + O.w + O.u
    val companionValue = "" + H.r + H.s + H.w + H.u
    return when {
        objectValue != "79a!x?!" -> "object: $objectValue"
        companionValue != "79a!x?!" -> "companion: $companionValue"
        else -> "OK"
    }
}
