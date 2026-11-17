// FILE: Declarations.kt
// A bounded type parameter's intersection solution publishes the bound across files.
interface Named { val name: String }
interface Marked
class First : Named, Marked { override val name get() = "first" }
class Second : Named, Marked { override val name get() = "second" }
class Box<out T>(val value: T)
fun <T : Named> pick(vararg values: T): T = values[0]
fun <T> any(vararg values: T): T = values[0]
fun <T : Named> boxed(vararg values: T): Box<T> = Box(values[0])
fun <T> keep(value: T): T = value
fun <T : Marked> mark(value: T): T = value
fun <T : Named> name(value: T): T = value
inline fun <R> within(block: () -> R): R = block()
fun picked() = pick(First(), Second())
fun unbounded() = any(First(), Second())
fun boxedPick() = boxed(First(), Second())
val property = pick(First(), Second())
fun kept() = keep(pick(First(), Second()))
fun wrapped() = Box(pick(First(), Second()))
fun local() = within { val value = pick(First(), Second()); value }
fun marked() = mark(pick(First(), Second()))
fun named() = name(pick(First(), Second()))
private fun hidden() = pick(First(), Second())
fun hiddenName() = hidden().name

// FILE: Box.kt
fun box(): String {
    val names = picked().name + property.name + kept().name + local().name
    val boxes = boxedPick().value.name + wrapped().value.name + hiddenName()
    val rebound = named().name
    marked()
    return if (names == "firstfirstfirstfirst" && boxes == "firstfirstfirst" && rebound == "first") "OK"
    else "fail $names $boxes $rebound"
}
