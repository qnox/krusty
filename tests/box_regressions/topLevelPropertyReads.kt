// A top-level property read has its declared type in every backend: a `val` with an
// initializer, a `var` written and read back, and a `val` typed as a supertype of its value.
interface Shape {
    fun name(): String
}

class Square : Shape {
    override fun name(): String = "square"
}

val greeting: String = "O"

var counter: Int = 0

val shape: Shape = Square()

val anything: Any = "K"

fun box(): String {
    counter += 2
    if (counter != 2) return "Fail counter"
    if (shape.name() != "square") return "Fail shape"
    if (anything !is String) return "Fail any"
    return greeting + anything
}
