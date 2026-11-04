// A supertype written with `W & Any` keeps that intersection when its class is applied to a
// nullable argument: `Holder<String?>` is a `Typed<String>`, directly, through a subclass, through
// an interface, and with a caller's type parameter as the argument.
interface Source<T> {
    fun read(): T
}

abstract class Typed<T> : Source<T>

open class Holder<W>(private val value: W & Any) : Typed<W & Any>() {
    override fun read(): W & Any = value
}

open class Derived<W>(value: W & Any) : Holder<W>(value)

fun direct(holder: Holder<String?>): Typed<String> = holder

fun inherited(holder: Derived<String?>): Typed<String> = holder

fun throughInterface(holder: Derived<String?>): Source<String> = holder

fun <W : Any> generic(holder: Derived<W?>): Typed<W> = holder

fun <W> open(holder: Derived<W>): Typed<W & Any> = holder

fun box(): String {
    val holder = Derived<String?>("O")
    if (direct(holder).read() != "O") return "Fail direct"
    if (inherited(holder).read() != "O") return "Fail inherited"
    if (throughInterface(holder).read() != "O") return "Fail interface"
    if (generic(Derived<String?>("K")).read() != "K") return "Fail generic"
    return open(holder).read() + generic(Derived<String?>("K")).read()
}
