// A library's top-level functions throw, catch by type, run `finally` on every exit, and use
// `try` as an expression. A program linked against the library's KLIB runs each body as the library
// wrote it.
// MODULE: lib
// FILE: Library.kt
package fixture

fun rethrow(error: Throwable): Nothing = throw error

fun checkedLength(text: String?): Int {
    if (text == null) throw IllegalArgumentException("no text")
    return text.length
}

fun lengthOrMinus(text: String?): Int {
    try {
        return checkedLength(text)
    } catch (expected: IllegalArgumentException) {
        return -1
    }
}

fun classify(error: Throwable): String =
    try {
        rethrow(error)
    } catch (state: IllegalStateException) {
        "state"
    } catch (argument: IllegalArgumentException) {
        "argument ${argument.message}"
    } catch (other: Throwable) {
        "other"
    }

fun trace(text: String?): String {
    var log = ""
    try {
        log = log + "try "
        log = log + checkedLength(text)
    } catch (expected: IllegalArgumentException) {
        log = log + "catch"
    } finally {
        log = log + " finally"
    }
    return log
}

fun finallyAfterReturn(text: String): String {
    var log = "start"
    try {
        return text
    } finally {
        log = log + " finally"
    }
}

fun nestedFinally(fail: Boolean): Int {
    var steps = 0
    try {
        try {
            steps = steps + 1
            if (fail) throw IllegalStateException("inner")
            steps = steps + 10
        } finally {
            steps = steps + 100
        }
    } catch (error: IllegalStateException) {
        steps = steps + 1000
    }
    return steps
}

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String {
    if (lengthOrMinus("four") != 4) return "lengthOrMinus four"
    if (lengthOrMinus(null) != -1) return "lengthOrMinus null"
    if (classify(IllegalStateException("s")) != "state") return "classify state"
    if (classify(IllegalArgumentException("a")) != "argument a") return "classify argument"
    if (classify(UnsupportedOperationException("u")) != "other") return "classify other"
    if (trace("ab") != "try 2 finally") return "trace ab: " + trace("ab")
    if (trace(null) != "try catch finally") return "trace null: " + trace(null)
    if (finallyAfterReturn("value") != "value") return "finallyAfterReturn"
    if (nestedFinally(false) != 111) return "nestedFinally false"
    if (nestedFinally(true) != 1101) return "nestedFinally true"
    try {
        rethrow(IllegalStateException("thrown"))
        return "rethrow returned"
    } catch (expected: IllegalStateException) {
        if (expected.message != "thrown") return "rethrow message"
    }
    return "OK"
}
