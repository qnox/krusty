// A `Unit` parameter is an ordinary parameter: its argument is evaluated for its effects and the
// callee receives the `Unit` instance, whether the call is direct, private, virtual through an
// override, or through an interface member whose type parameter is `Unit`.

interface Reader<T> {
    fun read(input: T): String
}

open class Base {
    open fun accept(marker: Unit, suffix: String): String = "base$suffix"
}

class Derived : Base() {
    override fun accept(marker: Unit, suffix: String): String = "derived" + describe(marker) + suffix
}

class UnitReader : Reader<Unit> {
    override fun read(input: Unit): String = describe(input)
}

fun describe(value: Any): String = value.toString()

private class Holder {
    private fun readObject(input: Unit): Unit = input

    fun call(): Any = readObject(Unit)
}

fun echo(value: Unit): Any = value

var effects = 0

fun effect() {
    effects++
}

fun box(): String {
    if (describe(echo(Unit)) != "kotlin.Unit") return "fail echo"
    if (describe(Holder().call()) != "kotlin.Unit") return "fail private"
    val base: Base = Derived()
    val accepted = base.accept(effect(), "!")
    if (accepted != "derivedkotlin.Unit!") return "fail override: $accepted"
    if (effects != 1) return "fail effects: $effects"
    val reader: Reader<Unit> = UnitReader()
    val read = reader.read(Unit)
    if (read != "kotlin.Unit") return "fail erased: $read"
    return "OK"
}
