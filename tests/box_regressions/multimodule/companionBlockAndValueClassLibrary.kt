// KOTLINC_VERSION: [2.4.20,)
// Earlier JVM reference compilers fail on the consumer module's call of a library
// companion-block function ("Fake override should have at least one overridden descriptor").
// LANGUAGE: +CompanionBlocksAndExtensions
// A library whose sources need the case's language features and source placeholders: every
// reference compiles each module under the `// LANGUAGE:` features the case names, with
// `OPTIONAL_JVM_INLINE_ANNOTATION` expanded for its backend.
// MODULE: lib
// FILE: Lib.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class W(val value: Int)

class C {
    companion {
        fun twice(x: Int): Int = x * 2
    }
}

fun wrap(x: Int): W = W(x)

// MODULE: main(lib)
// FILE: Main.kt
fun box(): String {
    val r = C.twice(wrap(3).value)
    return if (r == 6) "OK" else "r = $r"
}
