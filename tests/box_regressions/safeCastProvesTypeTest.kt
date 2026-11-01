// A non-null `x as? T` proves `x is T`, so `x` smart-casts after each form below.
class Holder(val value: String)

fun stop(): Nothing {
    while (true) {
    }
}

fun notNull(x: Any?): String = if ((x as? Holder) != null) x.value else "-"

fun elvisReturn(x: Any?): String {
    (x as? Holder) ?: return "-"
    return x.value
}

fun elvisNothing(x: Any?): String {
    (x as? Holder) ?: stop()
    return run { x.value + "1" }
}

fun safeCallChain(x: Any?): String {
    val length = (x as? Holder)?.value?.length ?: return "-"
    return x.value + length
}

fun nullableTarget(x: Any?): String {
    (x as? Holder?) ?: return "-"
    return x.value
}

fun box(): String {
    if (notNull(Holder("a")) != "a" || notNull("a") != "-" || notNull(null) != "-") return "notNull"
    if (elvisReturn(Holder("b")) != "b" || elvisReturn(1) != "-") return "elvisReturn"
    if (elvisNothing(Holder("c")) != "c1") return "elvisNothing"
    if (safeCallChain(Holder("dd")) != "dd2" || safeCallChain(null) != "-") return "safeCallChain"
    if (nullableTarget(Holder("e")) != "e" || nullableTarget(null) != "-") return "nullableTarget"
    return "OK"
}
