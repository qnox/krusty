import kotlin.reflect.KProperty

class Holder

class Delegate<T>(val value: T?, val lambda: (Holder.() -> T?)?) {
    operator fun getValue(thisRef: Holder, property: KProperty<*>): T? =
        if (lambda != null) thisRef.lambda() else value
    fun render(): String = if (lambda != null) "lambda" else "value"
}

class Keys {
    companion object {
        fun <T> key(defaultValue: T? = null, isTransient: Boolean = false): Delegate<T> =
            Delegate(defaultValue, null)

        fun <T> key(getDefaultValue: Holder.() -> T?, isTransient: Boolean = false): Delegate<T> =
            Delegate(null, getDefaultValue)
    }
}

val Holder.values: Sequence<String>? by Keys.key<Sequence<String>>(emptySequence())
val Holder.computed: String? by Keys.key<String>({ "computed" })
val viaValue: Delegate<Sequence<String>> = Keys.key<Sequence<String>>(emptySequence())
val viaLambda: Delegate<String> = Keys.key<String>({ "lambda" })

fun box(): String {
    val local = Keys.key<List<String>>(emptyList())
    if (local.render() != "value") return "FAIL local: ${local.render()}"
    if (viaValue.render() != "value") return "FAIL value: ${viaValue.render()}"
    if (viaLambda.render() != "lambda") return "FAIL lambda: ${viaLambda.render()}"
    if (Holder().values?.count() != 0) return "FAIL values"
    if (Holder().computed != "computed") return "FAIL computed"
    return "OK"
}
