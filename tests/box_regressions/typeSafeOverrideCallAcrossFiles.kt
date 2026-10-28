// A call to an override of a type-safe collection member declared in another file goes through
// the member's erased signature; a primitive argument is boxed for it.
// FILE: numbers.kt
class Numbers : HashSet<Int>() {
    override fun remove(element: Int): Boolean = super.remove(element)
}
// FILE: main.kt
fun removeNumber(numbers: Numbers): Boolean = numbers.remove(1)
fun box(): String {
    val numbers = Numbers()
    numbers.add(1)
    if (!removeNumber(numbers) || numbers.contains(1)) return "remove"
    return "OK"
}
