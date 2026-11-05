// A class type parameter's bound resolves before any rung of the class itself: the classifier
// nested in the class body does not hide the top-level `Limit` the bound names.
open class Limit {
    open fun tag(): String = "O"
}

class Own<T : Limit>(val bound: T) {
    open class Limit {
        fun tag(): String = "nested"
    }

    fun f(t: T): String = t.tag()
}

fun box(): String {
    val own = Own(object : Limit() {
        override fun tag(): String = "OK"
    })
    return own.f(own.bound)
}
