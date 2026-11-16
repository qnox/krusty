// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: Kotlin accepts `$` in a backticked name only on the JVM; Native and JS reject
// it as an illegal character.
// An inlined catch parameter or local named with `$` keeps that spelling and gains one `$iv` per
// expansion, without mangling the `$` it was declared with.
inline fun guarded(block: () -> Int): Int {
    try {
        return block()
    } catch (`p$q`: RuntimeException) {
        return -4
    }
}

inline fun outer(block: () -> Int): Int = guarded(block)

inline fun locals(block: (Int) -> Int): Int {
    val `p$q` = 2
    return block(`p$q`)
}

fun box(): String {
    val once = guarded { throw ArithmeticException() }
    val twice = outer { throw ArithmeticException() }
    val values = locals { `l$m` -> `l$m` }
    return if (once == -4 && twice == -4 && values == 2) "OK" else "fail"
}
