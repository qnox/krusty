// A labeled return from an inlined Unit lambda leaves that lambda's frame.
// `return@outer` from inside a nested lambda uses the outer frame: the statements after the inner
// call do not run, and a `finally` the return leaves does. `return@inner` stays in the outer
// lambda. A value-producing `also` still yields its receiver when the lambda returns early.
inline fun unitRun(block: () -> Unit) {
    block()
}

fun nested(): String {
    var log = ""
    unitRun outer@ {
        unitRun inner@ {
            if (true) return@outer
            log += "inner"
        }
        log += "side"
    }
    return log
}

fun stays(): String {
    var log = ""
    unitRun outer@ {
        unitRun inner@ {
            if (true) return@inner
            log += "inner"
        }
        log += "side"
    }
    return log
}

fun cleanup(): String {
    var log = ""
    unitRun outer@ {
        try {
            unitRun {
                if (true) return@outer
            }
        } finally {
            log += "finally"
        }
        log += "side"
    }
    return log
}

fun produced(value: String): String {
    var seen = "untouched"
    val result = value.also {
        if (it.length > 1) return@also
        seen = "ran"
    }
    return if (seen == "untouched") result else seen
}

fun box(): String {
    if (nested() != "") return "fail nested:${nested()}"
    if (stays() != "side") return "fail stays:${stays()}"
    if (cleanup() != "finally") return "fail cleanup:${cleanup()}"
    if (produced("ab") != "ab") return "fail also:${produced("ab")}"
    return "OK"
}
