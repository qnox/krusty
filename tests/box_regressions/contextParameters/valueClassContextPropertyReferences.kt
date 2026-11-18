// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual

import kotlin.jvm.JvmInline
import kotlin.reflect.KMutableProperty1
import kotlin.reflect.KProperty0
import kotlin.reflect.KProperty1

@JvmInline
value class Z(val value: String)

@JvmInline
value class N(val value: Int)

var log = ""

context(z: Z)
val decorated: String
    get() = "[" + z.value + "]"

context(z: Z, n: N)
val pair: String
    get() = z.value + n.value

class Box(val tag: String) {
    context(z: Z)
    val member: String
        get() = tag + z.value

    context(n: N)
    var counted: Int
        get() = n.value
        set(value) {
            log += "m$value${n.value};"
        }
}

context(z: Z)
val Box.label: String
    get() = tag + z.value

context(n: N)
var Box.extVar: Int
    get() = n.value + tag.length
    set(value) {
        log += "e$value${n.value}$tag;"
    }

fun box(): String {
    context(Z("O"), N(3)) {
        val top: KProperty0<String> = ::decorated
        if (top.get() != "[O]") return "FAIL top: ${top.get()}"
        val both: KProperty0<String> = ::pair
        if (both.get() != "O3") return "FAIL pair: ${both.get()}"
        val member: KProperty1<Box, String> = Box::member
        if (member.get(Box("K")) != "KO") return "FAIL member: ${member.get(Box("K"))}"
        val bound: KProperty0<String> = Box("B")::member
        if (bound.get() != "BO") return "FAIL bound: ${bound.get()}"
        val counted: KMutableProperty1<Box, Int> = Box::counted
        if (counted.get(Box("x")) != 3) return "FAIL counted: ${counted.get(Box("x"))}"
        counted.set(Box("x"), 9)
        val label: KProperty1<Box, String> = Box::label
        if (label.get(Box("L")) != "LO") return "FAIL label: ${label.get(Box("L"))}"
        val extVar: KMutableProperty1<Box, Int> = Box::extVar
        if (extVar.get(Box("ab")) != 5) return "FAIL extVar: ${extVar.get(Box("ab"))}"
        extVar.set(Box("ab"), 1)
    }
    if (log != "m93;e13ab;") return "FAIL log: $log"
    return "OK"
}
