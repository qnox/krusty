// A local or anonymous class implements an abstract member of a superclass declared in another
// file with an override whose result its expression body infers.

// FILE: base.kt
abstract class Source {
    abstract fun next(): Boolean
}

abstract class Adapter<T> {
    abstract fun read(source: Source): T
    abstract fun write(value: T): String
}

// FILE: main.kt
class Constant(private val value: Boolean) : Source() {
    override fun next() = value
}

val booleans: Adapter<Boolean> = object : Adapter<Boolean>() {
    override fun read(source: Source) = source.next()
    override fun write(value: Boolean) = value.toString()
}

fun negated(): Adapter<Boolean> {
    class Negated : Adapter<Boolean>() {
        override fun read(source: Source) = !source.next()
        override fun write(value: Boolean) = "not $value"
    }
    return Negated()
}

fun box(): String {
    if (!booleans.read(Constant(true))) return "fail: anonymous read"
    if (booleans.write(false) != "false") return "fail: anonymous write"
    if (negated().read(Constant(true))) return "fail: local read"
    if (negated().write(true) != "not true") return "fail: local write"
    return "OK"
}
