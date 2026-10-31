// A member extension's formal that the receiver reaches only covariantly (`Holder<out T>`) takes
// the common supertype of the receiver's and an argument's types, as a top-level extension does:
// `Holder<Sub>.same(base)` solves `T = Base`. An argument that reaches the formal contravariantly
// (`Consumer<T>`) is an upper bound and leaves the receiver's `T = Sub` in place.
open class Base(val name: String)
class Sub(name: String) : Base(name)
class Other(name: String) : Base(name)

class Holder<out T>(val value: T)

interface Consumer<in T> {
    fun accept(item: T)
}

class Recorder : Consumer<Base> {
    var seen = ""

    override fun accept(item: Base) {
        seen = item.name
    }
}

class Checks {
    fun <T : Any> Holder<T>.same(expected: T): Boolean = value == expected

    fun <T : Any> Holder<T>.pick(other: T, first: Boolean): T = if (first) value else other

    fun <T : Any> Holder<T>.consume(consumer: Consumer<T>): T {
        consumer.accept(value)
        return value
    }

    fun consumed(holder: Holder<Sub>, consumer: Consumer<Base>): Sub = holder.consume(consumer)

    fun sameAsBase(holder: Holder<Sub>, base: Base): Boolean = holder.same(base)

    fun picked(holder: Holder<Sub>, other: Other, first: Boolean): String =
        holder.pick(other, first).name

    fun Holder<Sub>.implicitlySame(base: Base): Boolean = same(base)

    fun withImplicitReceiver(holder: Holder<Sub>, base: Base): Boolean = holder.implicitlySame(base)
}

fun box(): String {
    val sub = Sub("s")
    val holder = Holder(sub)
    val checks = Checks()
    if (!checks.sameAsBase(holder, sub)) return "same"
    if (checks.sameAsBase(holder, Base("b"))) return "different"
    if (checks.picked(holder, Other("o"), true) != "s") return "pick first"
    if (checks.picked(holder, Other("o"), false) != "o") return "pick other"
    if (!checks.withImplicitReceiver(holder, sub)) return "implicit receiver"
    val recorder = Recorder()
    if (checks.consumed(holder, recorder).name != "s" || recorder.seen != "s") return "consume"
    return "OK"
}
