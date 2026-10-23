// MODULE: lib
package accessorproof

var accessorTrace = ""
var stored = 3

var Int.adjusted: Int
    get() {
        accessorTrace += "g"
        return this + stored
    }
    set(value) {
        accessorTrace += "s"
        stored = value - this
    }

val report: String get() = accessorTrace
inline fun serializedRead(value: Int): Int = value.adjusted

// MODULE: main(lib)
import accessorproof.adjusted
import accessorproof.report
import accessorproof.serializedRead

var operandTrace = ""

fun receiver(): Int {
    operandTrace += "r"
    return 4
}

fun value(): Int {
    operandTrace += "v"
    return 12
}

fun box(): String {
    receiver().adjusted = value()
    if (operandTrace != "rv") return "setter operand order"
    if (receiver().adjusted != 12) return "getter result"
    if (operandTrace != "rvr") return "getter evaluated twice"
    if (serializedRead(receiver()) != 12) return "serialized getter"
    if (operandTrace != "rvrr") return "serialized receiver evaluated twice"
    if (report != "sgg") return "accessor effects"
    return "OK"
}
