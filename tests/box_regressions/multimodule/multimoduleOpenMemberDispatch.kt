// A call from another module to an overridable member runs the receiver's override, whether the
// member is declared by an open class or by an interface with a default body.
// MODULE: lib
open class Base {
    open fun f(x: Int): Int = x
    fun twice(x: Int): Int = f(x) * 2
}
class Derived : Base() {
    override fun f(x: Int): Int = x + 1
}
interface Shape {
    fun sides(): Int = 0
}
class Square : Shape {
    override fun sides(): Int = 4
}
fun derived(): Base = Derived()
fun square(): Shape = Square()
// MODULE: main(lib)
fun box(): String {
    val base: Base = derived()
    if (base.f(1) != 2) return "fail: f ${base.f(1)}"
    if (base.twice(1) != 4) return "fail: twice ${base.twice(1)}"
    val shape: Shape = square()
    if (shape.sides() != 4) return "fail: sides ${shape.sides()}"
    return "OK"
}
