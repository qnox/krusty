// LANGUAGE: +ContextParameters
abstract class Named {
    context(prefix: String)
    abstract val label: Any
    context(prefix: String)
    fun describe(): String = label.toString()
}
class Short : Named() {
    context(prefix: String)
    override val label: String get() = prefix + "!"
}
fun box(): String {
    val named: Named = Short()
    with("ok") {
        if (named.describe() != "ok!") return "covariant: ${named.describe()}"
    }
    return "OK"
}
