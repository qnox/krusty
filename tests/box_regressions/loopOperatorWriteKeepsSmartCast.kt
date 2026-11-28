// An operator write of a variable inside a loop (`i++`, `i += 1`) keeps the smart cast the variable
// had before the loop, so the loop condition and body still read it at the narrowed type. Only a
// plain assignment in the loop ends the smart cast at the loop entry.
fun increment(): Int {
    var i: Any = 1
    if (i is Int) {
        while (i != 10) {
            i++
        }
        return i
    }
    return -1
}

fun compound(): Int {
    var s: Any = ""
    if (s is String) {
        while (s.length < 3) {
            s += "x"
        }
        return s.length
    }
    return -1
}

fun box(): String {
    if (increment() != 10) return "increment"
    if (compound() != 3) return "compound"
    return "OK"
}
