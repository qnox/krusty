// `(x as T).member()` calls through the cast's target type. A type test on `x` before the cast
// proves a flow intersection for `x` itself, which must not become the cast receiver's view.
interface Source<T>
abstract class Base<T> : Source<T>
interface Transformer<U, W> {
    fun unwrap(value: W): U
}

open class Wrap<U, W>(val delegate: Source<U & Any>, val transformer: Transformer<U, W>) : Base<W & Any>() {
    open fun unwrapRecursive(value: W?): Any? {
        val local = delegate
        return if (local is Wrap<*, *>) {
            (local as Wrap<Any, U>).unwrapRecursive(transformer.unwrap(value as W))
        } else {
            value?.let { transformer.unwrap(value) }
        }
    }

    open fun unwrapThroughProperty(value: W?): Any? {
        return if (delegate is Wrap<*, *>) {
            (delegate as Wrap<Any, U>).unwrapThroughProperty(transformer.unwrap(value as W))
        } else {
            value?.let { transformer.unwrap(value) }
        }
    }
}

class Plain : Source<String>

class Mark(private val mark: String) : Transformer<String, String> {
    override fun unwrap(value: String): String = value + mark
}

fun box(): String {
    val inner = Wrap<String, String>(Plain(), Mark("K"))
    val outer = Wrap<String, String>(inner, Mark("O"))
    if (outer.unwrapRecursive("") != "OK") return "Fail local"
    if (outer.unwrapThroughProperty("") != "OK") return "Fail property"
    return "OK"
}
