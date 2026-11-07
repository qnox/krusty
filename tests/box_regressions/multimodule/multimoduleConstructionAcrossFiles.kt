// Two files of one module each declare a class and an object. Another module constructs both
// classes and reads both objects, each through the entry its own file publishes.
// MODULE: lib
// FILE: first.kt
class First(private val text: String) {
    fun value(): String = text
}
object FirstObject {
    fun value(): String = "O"
}
// FILE: second.kt
class Second(private val text: String) {
    fun value(): String = text
}
object SecondObject {
    fun value(): String = "K"
}
// MODULE: main(lib)
// FILE: main.kt
fun box(): String {
    if (First("O").value() + Second("K").value() != "OK") return "fail: construction"
    return FirstObject.value() + SecondObject.value()
}
