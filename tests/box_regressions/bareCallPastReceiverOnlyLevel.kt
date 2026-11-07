// A same-package extension needs a receiver, so a bare call with none passes over it to the
// star-imported function of the same name. With an implicit receiver in scope, the extension is
// still selected.
// FILE: picks.kt
package picks

fun pick(value: Int): String = "imported $value"

// FILE: main.kt
import picks.*

class Logger {
    var log = ""
}

fun Logger.pick(value: Int): String {
    log += value
    return "extension $value"
}

fun box(): String {
    if (pick(1) != "imported 1") return "bare pick"
    val logger = Logger()
    if (logger.pick(2) != "extension 2") return "explicit pick"
    val implicit = with(logger) { pick(3) }
    if (implicit != "extension 3" || logger.log != "23") return "implicit pick"
    return "OK"
}
