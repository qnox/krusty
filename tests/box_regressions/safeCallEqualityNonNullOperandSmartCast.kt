// `receiver?.member == value` holds only when the safe call completed, so a `value` that cannot
// be null proves the receiver non-null on that side, whichever side the safe call is written.
class Crash(val simulator: String, val pid: Int)

fun matches(parsed: Crash?, simulator: String, pid: Int): Boolean =
    parsed?.simulator == simulator && parsed.pid == pid

fun reversed(parsed: Crash?, simulator: String): Int =
    if (simulator == parsed?.simulator) parsed.pid else -1

fun box(): String {
    if (!matches(Crash("sim", 7), "sim", 7)) return "fail matches"
    if (matches(null, "sim", 7)) return "fail null"
    if (matches(Crash("other", 7), "sim", 7)) return "fail other"
    if (reversed(Crash("sim", 3), "sim") != 3) return "fail reversed"
    if (reversed(null, "sim") != -1) return "fail reversed null"
    return "OK"
}
