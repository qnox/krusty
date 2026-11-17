// FILE: C.kt
// LANGUAGE: +CompanionBlocksAndExtensions
OPTIONAL_JVM_INLINE_ANNOTATION
value class W(val value: Int)
class C {
    companion {
        fun makes(): W = W(1)
    }
}
// FILE: Main.kt
// LANGUAGE: +CompanionBlocksAndExtensions
fun box(): String = if (C.makes().value == 1) "OK" else "fail"
