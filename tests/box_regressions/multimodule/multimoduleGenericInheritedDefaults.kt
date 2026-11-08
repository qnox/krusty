// MODULE: lib
open class Base<T>(val seed: T) {
    open fun f(x: T = seed): T = x
}
class Derived : Base<Int>(9) {
    override fun f(x: Int): Int = x + 1
}
// MODULE: main(lib)
fun box(): String = if (Derived().f() == 10) "OK" else "fail"
