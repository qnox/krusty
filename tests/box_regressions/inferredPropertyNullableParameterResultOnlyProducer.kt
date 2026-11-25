class Box<T>(val value: T?)

fun <T> boxOf(defaultValue: List<T>? = null): Box<T> = Box(defaultValue?.firstOrNull())

val fromEmptyList = boxOf<String>(emptyList())
val fromListOf = boxOf<String>(listOf())

fun inferred() = boxOf<String>(emptyList())

fun box(): String {
    if (fromEmptyList.value != null) return "FAIL emptyList"
    if (fromListOf.value != null) return "FAIL listOf"
    if (inferred().value != null) return "FAIL inferred"
    return checkDelegates()
}

class Holder

class Delegate<T>(val value: T?, val lambda: (Holder.() -> T?)?) {
    operator fun getValue(thisRef: Holder, property: kotlin.reflect.KProperty<*>): T? =
        if (lambda != null) thisRef.lambda() else value
}

class Keys {
    companion object {
        fun <T> key(defaultValue: T? = null, isTransient: Boolean = false): Delegate<T> =
            Delegate(defaultValue, null)

        fun <T> key(getDefaultValue: Holder.() -> T?, isTransient: Boolean = false): Delegate<T> =
            Delegate(null, getDefaultValue)
    }
}

val Holder.analysisDiagnostics by Keys.key<Sequence<String>>(emptySequence())
val Holder.computed by Keys.key<String>({ "computed" })

fun checkDelegates(): String {
    if (Holder().analysisDiagnostics?.count() != 0) return "FAIL delegate value"
    if (Holder().computed != "computed") return "FAIL delegate lambda"
    return "OK"
}
