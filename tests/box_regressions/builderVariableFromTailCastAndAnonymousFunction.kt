// A builder variable is inferred from a builder lambda's discarded tail typed by it (the tail
// constrains it to the lambda's `Unit` result), from a bare member call after a cast of `this`
// (the cast keeps the declared receiver), and from an anonymous function's declared return type
// through its expression body.
open class Buildee<TV>(private val value: Any?) {
    @Suppress("UNCHECKED_CAST")
    fun get(): TV = value as TV
}

class Derived<TA>(value: Any?) : Buildee<TA>(value)

class Target(val name: String)

fun <PTV> build(value: Any?, instructions: Buildee<PTV>.() -> Unit): Buildee<PTV> =
    Derived<PTV>(value).apply(instructions)

var seen = ""

fun consume(value: Any?) {
    seen += value
}

fun tail() = build(Unit) { get() }

fun cast() = build(Unit) { this as Derived<*>; get() }

fun anonymous() = build(Target("anonymous")) { fun(): Buildee<Target> = this }

fun used() = build(1) { this as Derived<*>; val value = get(); consume(value) }

fun box(): String {
    val unitTail: Buildee<Unit> = tail()
    val unitCast: Buildee<Unit> = cast()
    if (unitTail !is Derived<*> || unitCast !is Derived<*>) return "builder result"
    if (anonymous().get().name != "anonymous") return "anonymous function"
    used()
    if (seen != "1") return "used: $seen"
    return "OK"
}
