// WITH_STDLIB

open class X(val fn: () -> Unit)
open class C(val x: X)
class B(var value: Int) {
    fun update(): C = object : C(object : X({ value = 3 }) {}) {}
}

fun box(): String {
    val first = B(1)
    val other = B(2)
    val firstCopy = first.update()
    val otherCopy = other.update()
    if (first.value != 1 || other.value != 2) return "construction invoked the lambda"
    firstCopy.x.fn()
    if (first.value != 3 || other.value != 2) return "first receiver"
    otherCopy.x.fn()
    return if (first.value == 3 && other.value == 3) "OK" else "second receiver"
}
