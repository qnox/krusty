// A class type parameter's bound resolves before any rung of the class itself: the classifier
// nested in the supertype does not hide the top-level `Limit` the bound names.
open class Limit {
    open fun tag(): String = "O"
}

open class Base {
    open class Limit {
        fun tag(): String = "nested"
    }
}

class Child<T : Limit>(val bound: T) : Base() {
    fun f(t: T): String = t.tag()
}

fun box(): String {
    val child = Child(object : Limit() {
        override fun tag(): String = "OK"
    })
    return child.f(child.bound)
}
