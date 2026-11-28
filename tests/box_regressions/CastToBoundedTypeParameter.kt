open class C
interface I1
interface I2

class CI1I2 : C(), I1, I2
class OtherCI1I2 : C(), I1, I2

fun <R : C> Any?.castToClassBound(): R where R : I1, R : I2 {
    @Suppress("UNCHECKED_CAST")
    return this as R
}

fun <R : C> Any?.checkOnly() where R : I1, R : I2 {
    this as R
}

fun <T : I1> Any?.castToInterfaceBound(): T {
    @Suppress("UNCHECKED_CAST")
    return this as T
}

fun <T> Any?.castToUnbounded(): T {
    @Suppress("UNCHECKED_CAST")
    return this as T
}

fun failsWithClassCast(block: () -> Any?): Boolean =
    try {
        block()
        false
    } catch (e: ClassCastException) {
        true
    }

fun box(): String {
    CI1I2().checkOnly<CI1I2>()
    CI1I2().checkOnly<OtherCI1I2>()
    if (!failsWithClassCast { Any().checkOnly<CI1I2>() }) return "Any checked against the bound"
    if (!failsWithClassCast { Any().castToClassBound<CI1I2>() }) return "Any as class bound"
    if (!failsWithClassCast { "text".castToInterfaceBound<I1>() }) return "String as interface bound"
    if (failsWithClassCast { CI1I2().castToInterfaceBound<I1>() }) return "interface bound"
    return "OK"
}
