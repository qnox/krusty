import kotlin.jvm.JvmInline
import kotlin.reflect.KMutableProperty0
import kotlin.reflect.KProperty
import kotlin.reflect.KProperty0
import kotlin.reflect.KProperty1

@JvmInline
value class Z(val value: String)

@JvmInline
value class I(val value: Int)

class Box(val tag: String)

val written: Z
    get() = Z("W")

class Holder(val value: Z) {
    operator fun getValue(thisRef: Any?, property: KProperty<*>): Z = value
}

val delegated: Z by Holder(Z("D"))

var stored: Z = Z("S")

var custom: Z
    get() = stored
    set(value) {
        stored = value
    }

val nullable: I?
    get() = I(4)

val Box.wrapped: Z
    get() = Z(tag)

val Z.same: Z
    get() = this

fun box(): String {
    val w: KProperty0<Z> = ::written
    if (w.get().value != "W") return "FAIL written: ${w.get()}"
    val d: KProperty0<Z> = ::delegated
    if (d.get().value != "D") return "FAIL delegated: ${d.get()}"
    val c: KMutableProperty0<Z> = ::custom
    c.set(Z("C"))
    if (c.get().value != "C" || stored.value != "C") return "FAIL custom: ${c.get()} $stored"
    val n: KProperty0<I?> = ::nullable
    if (n.get()?.value != 4) return "FAIL nullable: ${n.get()}"
    val e: KProperty1<Box, Z> = Box::wrapped
    if (e.get(Box("E")).value != "E") return "FAIL extension: ${e.get(Box("E"))}"
    val s: KProperty1<Z, Z> = Z::same
    if (s.get(Z("K")).value != "K") return "FAIL value-class receiver: ${s.get(Z("K"))}"
    return "OK"
}
