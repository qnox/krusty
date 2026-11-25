// FILE: ValueClassBlock.kt
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
fun box(): String {
    val r = W(1).sum()
    return if (r == 2 + 1 + 3 && W.one == 1) "OK" else "r = $r"
}
