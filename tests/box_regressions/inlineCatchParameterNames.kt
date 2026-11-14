// An inlined value keeps the name it was declared with and gains one `$iv` per expansion:
// an unused catch parameter is `<unused var>$iv`, and a named or backticked one is not mangled.
inline fun guarded(block: () -> Int): Int {
    try {
        return block()
    } catch (_: IllegalStateException) {
        return -1
    } catch (e_x: IllegalArgumentException) {
        return -2
    } catch (`a b`: UnsupportedOperationException) {
        return -3
    } catch (`p$q`: RuntimeException) {
        return -4
    }
}

inline fun outer(block: () -> Int): Int = guarded(block)

inline fun locals(block: (Int) -> Int): Int {
    val `x-y` = 1
    val `p$q` = 2
    return block(`x-y` + `p$q`)
}

fun box(): String {
    val unused = guarded { error("unused") }
    val named = outer { throw IllegalArgumentException() }
    val spaced = guarded { throw UnsupportedOperationException() }
    val dollar = outer { throw ArithmeticException() }
    val local = guarded {
        try { "local".toInt() } catch (_: NumberFormatException) { 7 }
    }
    val values = locals { `l-m` -> `l-m` }
    return if (unused == -1 && named == -2 && spaced == -3 && dollar == -4 && local == 7 &&
        values == 3
    ) "OK" else "fail"
}
