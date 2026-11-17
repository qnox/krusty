// FILE: W.kt
// LANGUAGE: +CompanionBlocks
OPTIONAL_JVM_INLINE_ANNOTATION
value class W(val value: Int) {
    companion {
        val one get() = 1
        fun plain(x: Int) = x + 1
        fun takes(w: W) = w.value
        fun makes(): W = W(3)
    }
    fun sum() = plain(one) + takes(this) + makes().value
}
// FILE: Main.kt
// LANGUAGE: +CompanionBlocks
fun box(): String {
    val r = W.plain(W.takes(W.makes())) + W.one
    return if (r == 4 + 1) "OK" else "r = $r"
}
