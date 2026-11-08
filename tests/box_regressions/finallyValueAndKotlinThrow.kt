var finallyEffects = 0
var finallyTrace = ""

fun box(): String {
    val saved = try { finallyEffects + 1 } finally { finallyEffects = 9 }
    if (saved != 1 || finallyEffects != 9) return "saved value"

    val original = Throwable("original")
    val replacement = Throwable("replacement")
    finallyEffects = 0
    val rethrown = try {
        try { throw original } finally { finallyEffects += 1 }
    } catch (caught: Throwable) { caught }
    if (rethrown !== original || finallyEffects != 1) return "original exception"

    finallyEffects = 0
    val replaced = try {
        try { throw original } finally {
            finallyEffects += 1
            throw replacement
        }
    } catch (caught: Throwable) { caught }
    if (replaced !== replacement || finallyEffects != 1) return "cleanup exception"

    finallyEffects = 0
    val catchThrow = try {
        try { throw original } catch (caught: Throwable) {
            throw replacement
        } finally { finallyEffects += 1 }
    } catch (caught: Throwable) { caught }
    if (catchThrow !== replacement || finallyEffects != 1) return "catch cleanup"

    finallyEffects = 0
    val normalThrow = try {
        try { "unused" } finally {
            finallyEffects += 1
            throw replacement
        }
    } catch (caught: Throwable) { caught }
    if (normalThrow !== replacement || finallyEffects != 1) return "normal cleanup throw"

    finallyTrace = ""
    val nested = try {
        try { "OK" } finally { finallyTrace += "I" }
    } finally { finallyTrace += "O" }
    if (nested != "OK" || finallyTrace != "IO") return "nested cleanup"

    val caughtValue = try { throw original } catch (caught: Throwable) {
        "OK"
    } finally { finallyTrace += "C" }
    return if (caughtValue == "OK" && finallyTrace == "IOC") "OK" else "catch value"
}
