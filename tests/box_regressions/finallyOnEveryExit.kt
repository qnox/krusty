// A `finally` block runs on every exit from its `try`: a `return` from the body or a catch, a
// `break` or `continue` out of a loop, including a labeled one past nested regions, and an
// exception. A `return` in the cleanup itself replaces the pending exit or exception.
var log = ""

fun returnFromBody(): String {
    try {
        log += "b"
        return "body"
    } finally {
        log += "f"
    }
}

fun returnFromCatch(): Int {
    try {
        log += "t"
        throw IllegalStateException("x")
    } catch (e: Throwable) {
        log += "c"
        return 2
    } finally {
        log += "f"
    }
}

fun cleanupReturnWins(): String {
    try {
        return "body"
    } finally {
        return "cleanup"
    }
}

fun cleanupReturnSwallows(): String {
    try {
        throw IllegalStateException("lost")
    } finally {
        return "swallowed"
    }
}

fun nestedReturn(): String {
    try {
        try {
            log += "i"
            return "nested"
        } finally {
            log += "1"
        }
    } finally {
        log += "2"
    }
}

fun breakAndContinue(): Int {
    var total = 0
    var i = 0
    while (true) {
        i++
        try {
            if (i == 2) continue
            if (i == 4) break
            total += i
        } finally {
            log += i
        }
    }
    return total
}

fun labeledBreak(): Int {
    var count = 0
    outer@ for (i in 0 until 3) {
        for (j in 0 until 3) {
            try {
                try {
                    if (j == 1) continue@outer
                    if (i == 2) break@outer
                    count++
                } finally {
                    log += "a"
                }
            } finally {
                log += "b"
            }
        }
    }
    return count
}

fun loopInsideTry(): Int {
    var total = 0
    try {
        for (i in 0 until 5) {
            if (i == 3) break
            total += i
        }
    } finally {
        log += "L"
    }
    return total
}

fun returnValueBeforeCleanup(): Int {
    var x = 1
    try {
        return x
    } finally {
        x = 5
        log += x
    }
}

inline fun guarded(x: String): String {
    try {
        log += x
        return log
    } finally {
        log += "f"
    }
}

fun box(): String {
    if (returnFromBody() != "body" || log != "bf") return "FAIL body: $log"
    log = ""
    if (returnFromCatch() != 2 || log != "tcf") return "FAIL catch: $log"
    if (cleanupReturnWins() != "cleanup") return "FAIL cleanup return"
    if (cleanupReturnSwallows() != "swallowed") return "FAIL swallow"
    log = ""
    if (nestedReturn() != "nested" || log != "i12") return "FAIL nested: $log"
    log = ""
    if (breakAndContinue() != 4 || log != "1234") return "FAIL loop: $log"
    log = ""
    if (labeledBreak() != 2 || log != "ababababab") return "FAIL labeled: $log"
    log = ""
    if (loopInsideTry() != 3 || log != "L") return "FAIL inner loop: $log"
    log = ""
    if (returnValueBeforeCleanup() != 1 || log != "5") return "FAIL value: $log"
    log = ""
    if (guarded("a") != "a" || log != "af") return "FAIL inline: $log"
    return "OK"
}
