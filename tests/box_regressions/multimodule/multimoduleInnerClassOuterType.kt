// MODULE: lib
class Outer<T>(private val outer: T) {
    inner class Inner<U>(private val inner: U) {
        fun innerValue(): U = inner
    }
}
fun value(): Outer<Any>.Inner<String> {
    val outer = Outer<Any>("O")
    return outer.Inner("K")
}
// MODULE: main(lib)
fun accept(value: Outer<Any>.Inner<String>): String = value.innerValue()
fun box(): String {
    val inner: String = accept(value())
    return if (inner == "K") "OK" else "fail $inner"
}
