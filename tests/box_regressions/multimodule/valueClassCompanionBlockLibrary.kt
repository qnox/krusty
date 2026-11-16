// MODULE: lib
// FILE: Lib.kt
// LANGUAGE: +CompanionBlocksAndExtensions
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
// MODULE: main(lib)
// FILE: Main.kt
// LANGUAGE: +CompanionBlocksAndExtensions
fun box(): String {
    val r = W.plain(W.takes(W.makes())) + W.one + W(1).sum()
    return if (r == 4 + 1 + 6) "OK" else "r = $r"
}
